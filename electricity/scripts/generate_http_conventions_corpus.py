#!/usr/bin/env python3
"""A differential corpus for `circuitry.http_conventions` (issue #778):
the proxy rule (including `no_proxy`'s exact-IP and
`host:port` matching), strict port parsing, the http(s)-only
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
# 1. `check_url` -- scheme detection, then strict ports.
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
        "http://host\0/x",
        "http://ho st/x",
        "http://host\t/x",
        "http://host\n/x",
        "http://host/\x7fx",
        # A refused scheme with a password hidden behind the
        # authority's own boundary (issue #778): the refusal must
        # never echo it.
        "ftp://user:pa/ss@host/x",
        "ftp://user:pa?ss@host/x",
        "ftp://user:pa#ss@host/x",
        # A control character or whitespace, with the same hidden
        # password (issue #778): checked before the scheme, but the
        # refusal must still never echo it.
        "http://user:pa/ss@host\0/x",
        "http://user:pa?ss@host\t/x",
        "http://user:pa#ss@host\n/x",
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
# 2. `check_strict_port`.
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
        "http://[::1]x/x",
        "http://host:0/x",
        "http://host:65536/x",
        "http://host:+9/x",
        "http://host:0_9/x",
        "http://host: 9/x",
        "http://user:p@ss@host:99999/x",
        "http://host:80#x",
        # A password hidden behind the authority's own boundary (issue
        # #778): the "port" is really a credential fragment, never
        # echoed in the refusal.
        "https://user:pa/ss@api.example.com/v1",
        "https://user:pa?ss@api.example.com/v1",
        "https://user:pa#ss@api.example.com/v1",
        # The same hidden password, but behind an authority that
        # already has userinfo of its own -- an email address used as
        # a username (issue #778). The refusal must still never echo
        # it, even though the netloc itself already contains an `@`.
        "https://me@corp.com:pa/ss@api.example.com/v1",
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
# 3. Userinfo/credential redaction -- `redact_request_url`, the one
#    function every refusal/warning/event/state/hint/`host:` text that
#    names a request URL on either engine uses (issue #778).
# ---------------------------------------------------------------------


def _redaction_cases() -> list[dict[str, Any]]:
    urls = [
        "http://user:pass@host/x",
        "http://host/x",
        "socks5://alice:p@ss@proxy:1080/x",
        "https://user:s3cr3t@host/x?api_key=sk-test",
        "https://host/x?api_key=sk-test&q=widgets",
        "https://host/x",
        # Hostile passwords (issue #778): a control character or
        # whitespace inside one must still be redacted in full.
        "http://alice:pa ss@host:1234/x",
        "http://alice:pa\rss@host:1234/x",
        "http://alice:pa:ss@host:1234/x",
        # A credential-like query key behind an `@` that follows the
        # authority (issue #778): the cut to the last `@` must run
        # *before* the query is masked, or the mask's own re-encoding of
        # a literal `@` as `%40` would hide it from the cut.
        "https://alice:443/x@api.example.com/v1?api_key=sk",
        # A URL with no `scheme://` prefix at all (issue #778): the
        # same shape as a schemeless proxy value, which
        # `redact_proxy_value` already handles -- the whole value is
        # searched for the LAST `@`, same as it would be.
        "alice:pw@host/x",
        "alice:pw@localhost:11434",
        # The same shape, but with a literal `://` elsewhere in the
        # value (issue #778): the parsed `netloc` is still empty, so
        # the whole value must still be cut at the LAST `@`, not just
        # left alone because `"://" in url` is true.
        "alice:pw@host/x?next=http://y",
        "alice:p://w@host/x",
    ]
    return [{"url": url, "redacted": http_conventions.redact_request_url(url)} for url in urls]


def _proxy_redaction_cases() -> list[dict[str, Any]]:
    values = [
        "http://alice:pass@proxy:3128",
        "proxy.corp:3128",
        # Hostile passwords (issue #778): a proxy value has no
        # meaningful path, so none of these end its authority early --
        # the whole value, after its scheme (or from the start, with
        # none), is searched for the LAST `@`.
        "http://alice:pa/ss@proxy:3128",
        "http://alice:pa?ss@proxy:3128",
        "http://alice:pa#ss@proxy:3128",
        "http://alice:pa ss@proxy:3128",
        "http://alice:pa\rss@proxy:3128",
        "http://alice:pa@ss@proxy:3128",
        "http://alice:pa:ss@proxy:3128",
        "alice:s3cret@proxy:3128 ",
    ]
    return [
        {"value": value, "redacted": http_conventions.redact_proxy_value(value)}
        for value in values
    ]


# ---------------------------------------------------------------------
# 4. `resolve_proxy`, including `no_proxy`'s exact-IP and
#    `host:port` matching (issue #778).
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
    # A `host:port` entry matches a URL's implicit default port too
    # (issue #778): `example.com:443` matches a portless
    # `https://example.com/`.
    add(
        {"https_proxy": "http://p:1", "no_proxy": "example.com:443"},
        "https",
        "example.com",
        None,
    )
    add(
        {"https_proxy": "http://p:1", "no_proxy": "example.com:443"},
        "https",
        "example.com",
        8443,
    )
    # A non-ASCII decimal digit in the entry's port text is never a
    # match (issue #778).
    add({"http_proxy": "http://p:1", "no_proxy": "example.com:²"}, "http", "example.com")
    add({"http_proxy": "socks5://host:1080"}, "http", "example.com")
    add({"http_proxy": "http://host:99999"}, "http", "example.com")
    add({}, "http", "example.com")
    # The authority ends at the first `/`, `?` or `#` (issue #778), so
    # these are refused -- an `@` follows it -- never a port-parsing
    # failure, and never a `/`-bounded value silently accepted either.
    add({"http_proxy": "http://alice:pa/ss@proxy:3128"}, "http", "example.com")
    add({"http_proxy": "http://alice:pa?ss@proxy:3128"}, "http", "example.com")
    add({"http_proxy": "http://alice:pa#ss@proxy:3128"}, "http", "example.com")
    # A common form (issue #778): a trailing `/` after an explicit
    # port still works, and a portless value still gets this module's
    # own default (80), never curl's historic SOCKS default (1080).
    add({"http_proxy": "http://proxy.corp:3128/"}, "http", "example.com")
    add({"http_proxy": "http://proxy.corp/"}, "http", "example.com")
    add({"http_proxy": "http://alice:pa:ss@proxy:3128"}, "http", "example.com")
    # A proxy value with a control character or whitespace is refused
    # with the shared text, same as a request URL (issue #778).
    add({"http_proxy": "http://alice:pa ss@proxy:3128"}, "http", "example.com")
    add({"http_proxy": "http://alice:pa\rss@proxy:3128"}, "http", "example.com")
    add({"http_proxy": "alice:s3cret@proxy:3128 "}, "http", "example.com")
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
        # Lenient forms `email.utils.parsedate_to_datetime` (and, before
        # this fix, `chrono::DateTime::parse_from_rfc2822` on the Rust
        # side) would accept, but RFC 9110's own exact grammar does not
        # (issue #778).
        "Sun, 06 Nov 1994 08:49:37 +0000",
        "06 Nov 1994 08:49:37 GMT",
        "Sun, 6 Nov 1994 08:49:37 GMT",
        "Sun, 06 Nov 94 08:49:37 GMT",
        "Sun, 06 Nov 1994 08:49:37 EST",
        "Sun Nov 6 08:49:37 1994",
        "Mon, 06-Nov-94 08:49:37 GMT",
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
        "proxy_redaction_cases": _proxy_redaction_cases(),
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
