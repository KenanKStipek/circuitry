#!/usr/bin/env python3
"""A differential corpus for `circuitry.http_conventions` (issue #778
step 7): H1's proxy rule (including `no_proxy`'s exact-IP and
`host:port` matching), H3's strict port parsing, H4's http(s)-only
scheme check, userinfo/credential redaction, and RFC 9110 `Retry-After`
parsing -- Circuitry's own conventions now, never urllib's or curl's,
so this corpus runs cof's own functions directly and lets electricity
replay the result, rather than comparing either engine against a
third-party reference.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, per every other `generate_*.py`). Usage:

    python3 generate_http_conventions_corpus.py [--check]
"""

from __future__ import annotations

import json
import math
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from circuitry import http_conventions  # noqa: E402
from circuitry.adapters._retry import parse_retry_after_seconds  # noqa: E402

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-tools"
    / "tests"
    / "golden"
    / "http_conventions_corpus.json"
)


# ---------------------------------------------------------------------
# 1. H4 (and H3): `check_url` -- scheme detection, then strict ports.
# ---------------------------------------------------------------------


def _scheme_detection_cases() -> list[dict[str, Any]]:
    urls = [
        "http://example.com/x",
        "https://example.com/x",
        "not a url",
        ":::",
        "javascript:alert(1)",
        "//example.com/x",
        "127.0.0.1:8080/x",
        "ftp://example.com/x",
        "FTP://example.com/x",
        "HTTP://EXAMPLE.COM/x",
        "http://user:pass@example.com/x",
    ]
    cases = []
    for url in urls:
        try:
            http_conventions.check_url(url)
            error = None
        except ValueError as exc:
            error = str(exc)
        cases.append({"url": url, "error": error})
    return cases


# ---------------------------------------------------------------------
# 2. H3: `check_strict_port`.
# ---------------------------------------------------------------------


def _strict_port_cases() -> list[dict[str, Any]]:
    urls = [
        "http://host/x",
        "http://host:80/x",
        "http://host:1/x",
        "http://host:65535/x",
        "http://host:/x",
        "http://[::1]/x",
        "http://[::1]:8080/x",
        # `http://[::1]x/x` is deliberately not in this table: cof's own
        # `urllib.parse.urlsplit` raises its own bare "Invalid IPv6
        # URL" there (CPython's own `_check_bracketed_netloc`, not any
        # text this module builds), which this corpus compares exactly
        # -- `strict_port`'s own ported text ("invalid port in URL
        # ...") is pinned directly in each engine's own unit tests
        # instead, since the two engines' texts for this one input are
        # not meant to match.
        "http://host:0/x",
        "http://host:65536/x",
        "http://host:+9/x",
        "http://host:0_9/x",
        "http://host: 9/x",
        "http://user:p@ss@host:99999/x",
        "http://host:80#x",
    ]
    cases = []
    for url in urls:
        try:
            http_conventions.check_strict_port(url)
            error = None
        except ValueError as exc:
            error = str(exc)
        cases.append({"url": url, "error": error})
    return cases


# ---------------------------------------------------------------------
# 3. Userinfo/credential redaction -- `redact_url`.
# ---------------------------------------------------------------------


def _redaction_cases() -> list[dict[str, Any]]:
    urls = [
        "http://user:pass@host/x",
        "http://host/x",
        "socks5://alice:p@ss@proxy:1080/x",
        "https://user:s3cr3t@host/x?api_key=sk-test",
        "https://host/x?api_key=sk-test&q=widgets",
        "https://host/x",
    ]
    return [{"url": url, "redacted": http_conventions.redact_url(url)} for url in urls]


# ---------------------------------------------------------------------
# 4. H1: `resolve_proxy`, including `no_proxy`'s exact-IP and
#    `host:port` matching (issue #778 step 7, H-review P2-13).
# ---------------------------------------------------------------------


def _proxy_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []

    def add(
        env: dict[str, str],
        scheme: str,
        host: str,
        port: int | None = None,
    ) -> None:
        try:
            result: dict[str, Any] = {"ok": http_conventions.resolve_proxy(
                scheme, host, env, port=port
            )}
        except ValueError as exc:
            result = {"error": str(exc)}
        cases.append(
            {
                "env": sorted(env.items()),
                "scheme": scheme,
                "host": host,
                "port": port,
                "result": result,
            }
        )

    add({"http_proxy": "http://p:1"}, "http", "example.com")
    add({"HTTP_PROXY": "http://p:1"}, "http", "example.com")
    add({"http_proxy": "http://lower:2", "HTTP_PROXY": "http://upper:1"}, "http", "example.com")
    add({"https_proxy": "http://p:1"}, "https", "example.com")
    add({"HTTPS_PROXY": "http://p:1"}, "https", "example.com")
    add({"all_proxy": "http://p:1"}, "http", "example.com")
    add({"ALL_PROXY": "http://p:1"}, "https", "example.com")
    add({"http_proxy": "proxy.corp:3128"}, "http", "example.com")
    add({"http_proxy": "proxy.corp"}, "http", "example.com")
    add({"http_proxy": "http://user:pass@proxy.corp:3128"}, "http", "example.com")
    add({"http_proxy": "http://user@proxy.corp:3128"}, "http", "example.com")
    add({"http_proxy": "http://p:1", "no_proxy": "*"}, "http", "example.com")
    add({"http_proxy": "http://p:1", "no_proxy": "example.com"}, "http", "example.com")
    add({"http_proxy": "http://p:1", "no_proxy": "example.com"}, "http", "api.example.com")
    add({"http_proxy": "http://p:1", "no_proxy": ".example.com"}, "http", "api.example.com")
    add({"http_proxy": "http://p:1", "no_proxy": "example.com"}, "http", "notexample.com")
    add({"http_proxy": "http://p:1", "no_proxy": "0.0.1"}, "http", "127.0.0.1")
    add({"http_proxy": "http://p:1", "no_proxy": "0.0.1"}, "http", "0.0.1")
    add(
        {"http_proxy": "http://p:1", "no_proxy": "example.com:8080"},
        "http",
        "example.com",
        8080,
    )
    add(
        {"http_proxy": "http://p:1", "no_proxy": "example.com:8080"},
        "http",
        "example.com",
        80,
    )
    add(
        {"http_proxy": "http://p:1", "no_proxy": "example.com:8080"},
        "http",
        "example.com",
        None,
    )
    add({"http_proxy": "socks5://host:1080"}, "http", "example.com")
    add({"http_proxy": "http://host:99999"}, "http", "example.com")
    add({}, "http", "example.com")
    return cases


# ---------------------------------------------------------------------
# 5. `Retry-After`, RFC 9110 section 10.2.3, a fixed `now`.
# ---------------------------------------------------------------------


def _retry_after_cases() -> list[dict[str, Any]]:
    now = datetime(2026, 1, 1, 0, 0, 0, tzinfo=timezone.utc)
    values = [
        "120",
        "0",
        "-30",
        "+30",
        "1.5",
        "inf",
        "Infinity",
        "nan",
        "1_000",
        "1 2",
        "",
        "not a date or a number",
        "1" * 400,
        "Sun, 06 Nov 1994 08:49:37 GMT",
        "Sun, 06 Nov 1994 08:49:37",
        "Sunday, 06-Nov-94 08:49:37 GMT",
        "Sunday, 06-Nov-94 08:49:37 UTC",
        "Sun Nov  6 08:49:37 1994",
        "Mon, 01 Jan 2024 00:00:00 GMT",
    ]
    cases = []
    for value in values:
        seconds = parse_retry_after_seconds(value, now=now)
        # `json.dumps` happily (non-standardly) emits a bare `Infinity`
        # token for `float("inf")`, which `serde_json` refuses to parse
        # -- a string sentinel travels through strict JSON instead.
        if seconds is not None and math.isinf(seconds):
            seconds = "inf"
        cases.append({"now": now.isoformat(), "value": value, "seconds": seconds})
    return cases


def build_corpus() -> dict[str, Any]:
    return {
        "scheme_detection_cases": _scheme_detection_cases(),
        "strict_port_cases": _strict_port_cases(),
        "redaction_cases": _redaction_cases(),
        "proxy_cases": _proxy_cases(),
        "retry_after_cases": _retry_after_cases(),
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
