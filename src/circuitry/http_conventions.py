"""HTTP conventions shared by the ``http`` tool and every curl-based
adapter (issue #778 step 7): one proxy rule, strict port parsing, and
refusing a scheme other than ``http``/``https`` before any I/O.

Circuitry decides the proxy itself here, rather than letting urllib's or
curl's own environment reading decide it independently per surface:

- An ``http`` URL uses ``http_proxy``, lowercase only; uppercase
  ``HTTP_PROXY`` is never read. An ``https`` URL uses ``https_proxy``, then
  ``HTTPS_PROXY``. Either falls back to ``all_proxy``, then ``ALL_PROXY``.
  When both cases of a variable are set, lowercase wins (there is only ever
  one lowercase form to begin with, since the uppercase one is read only as
  a fallback).
- ``no_proxy`` (then ``NO_PROXY``) is a comma-separated list; ``*`` bypasses
  everything, and an entry matches a host exactly or as a domain suffix (a
  leading dot is optional).
- A proxy value with no ``scheme://`` prefix means ``http://``; only an
  ``http`` proxy scheme is supported (matching curl's own CONNECT-based
  proxying, the only kind either engine implements) — anything else is
  refused the same way an unsupported request scheme is.

Every proxy-*deciding* function here (:func:`resolve_proxy`,
:func:`check_proxy_url`, ...) takes its environment as an explicit
mapping, never reading ``os.environ`` itself, so callers (and tests)
control it fully. The two convenience wrappers that actually *issue* a
request (:func:`apply_request_proxy`, :func:`open_with_proxy_rule`) read
the real process environment, since every caller they have wants that.
"""

from __future__ import annotations

import base64
import ipaddress
import os
import re
import urllib.parse
import urllib.request
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

#: The only two schemes a request URL (or an adapter's `base_url`) may use.
ALLOWED_SCHEMES = ("http", "https")

_PORT_RE = re.compile(r"^[0-9]+$")

# Greedy up to the *last* `@` before whitespace or a `/` -- an authority's
# userinfo can itself carry a literal `@` (in the password), so stopping
# at the first one (as a non-greedy or `[^@]+` pattern would) can leave
# part of a credential in the stripped text (issue #778 step 7, H-review
# P1-9).
_USERINFO_RE = re.compile(r"://[^\s/]*@")

#: Query-string keys that commonly carry a credential (search APIs, some
#: webhook/trigger URLs). Matched case-insensitively against the whole
#: key, so ``api_key``, ``apiKey``, ``access_token`` and ``X-Auth-Token``
#: all match.
_CREDENTIAL_QUERY_KEY_RE = re.compile(r"(key|token|secret|auth|password|pwd)", re.IGNORECASE)


def strip_url_userinfo(url: str) -> str:
    """*url* with any ``user[:pass]@`` userinfo removed, for an error
    message — never the raw value, which can carry a credential. Strips up
    to the *last* ``@`` inside the authority (before any ``/``, ``?``,
    ``#`` or whitespace), matching how a host is actually parsed, so a
    password containing ``@`` can never leave a fragment of itself behind.
    """
    return _USERINFO_RE.sub("://", url)


def mask_credential_query_params(url: str) -> str:
    """*url* with the value of any query-string parameter whose key looks
    like a credential masked, e.g. ``https://api.example.com/search?key=
    secret`` becomes ``...?key=%2A%2A%2A``."""
    parsed = urllib.parse.urlsplit(url)
    if not parsed.query:
        return url
    pairs = urllib.parse.parse_qsl(parsed.query, keep_blank_values=True)
    masked = [(k, "***" if _CREDENTIAL_QUERY_KEY_RE.search(k) else v) for k, v in pairs]
    new_query = urllib.parse.urlencode(masked)
    return urllib.parse.urlunsplit(parsed._replace(query=new_query))


def redact_url(url: str) -> str:
    """*url* with userinfo stripped, then any credential-like query
    parameter masked — the two masks every refusal message in this module
    applies before naming the URL."""
    return strip_url_userinfo(mask_credential_query_params(url))


def _host_port_text(netloc: str) -> str | None:
    """The raw port text in *netloc* (userinfo already stripped by the
    caller): ``None`` with no ``:`` separator at all; otherwise the
    (possibly empty) text after it. A bracketed IPv6 host's own colons are
    never split."""
    if netloc.startswith("["):
        end = netloc.find("]")
        rest = netloc[end + 1 :] if end != -1 else ""
    elif ":" in netloc:
        rest = ":" + netloc.partition(":")[2]
    else:
        return None
    if not rest.startswith(":"):
        return None
    return rest[1:]


def check_strict_port(url: str) -> None:
    """Refuses *url* before any I/O when its port isn't strictly valid
    (H3): after the host (or after ``]`` for IPv6) comes ``:`` and one or
    more ASCII digits, 1-65535. An empty port, or none at all, means the
    scheme's default and is never refused.
    """
    netloc = urllib.parse.urlsplit(url).netloc
    host_port = netloc.rpartition("@")[2]
    port_text = _host_port_text(host_port)
    if not port_text:
        return
    if _PORT_RE.fullmatch(port_text) and 1 <= int(port_text) <= 65535:
        return
    raise ValueError(f"invalid port in URL '{redact_url(url)}': '{port_text}'")


def check_url(url: str, *, allowed_schemes: tuple[str, ...] = ALLOWED_SCHEMES) -> None:
    """Refuses *url* before any I/O (H3, H4): its scheme must be one of
    *allowed_schemes*, and its port must pass :func:`check_strict_port`.
    """
    scheme = urllib.parse.urlsplit(url).scheme.lower()
    if scheme not in allowed_schemes:
        allowed = " or ".join(allowed_schemes)
        raise ValueError(
            f"unsupported URL scheme '{scheme}' in '{redact_url(url)}': "
            f"only {allowed} is supported"
        )
    check_strict_port(url)


def _normalize_proxy_url(value: str) -> str:
    """*value* with a scheme prefixed (``http://``) when it has none at
    all — H1's "a proxy URL with no scheme means http://"."""
    return value if "://" in value else f"http://{value}"


def _with_default_port(url: str, default_port: int) -> str:
    """*url* with ``:<default_port>`` written into its authority when it
    names no port of its own, not even an empty one (H3: an absent port
    means the scheme's own default). Every surface that reads this
    normalized proxy URL back out (curl's ``--config``, the ``http``
    tool's ``Request.set_proxy``) must see the real default explicitly,
    since curl's *own* default for a schemeless/portless proxy is 1080,
    its historic SOCKS-proxy convention — never H1's own 80 (issue #778
    step 7, H-review P1-6).
    """
    parts = urllib.parse.urlsplit(url)
    host_port = parts.netloc.rpartition("@")[2]
    if _host_port_text(host_port):
        return url
    netloc = parts.netloc if host_port.endswith(":") else f"{parts.netloc}:"
    return urllib.parse.urlunsplit(parts._replace(netloc=f"{netloc}{default_port}"))


def check_proxy_url(value: str) -> str:
    """*value*, normalized (H1), with its scheme and port checked before
    any I/O: only an ``http`` proxy scheme is supported, and its port
    follows :func:`check_strict_port`. The returned URL always names a
    port explicitly (:func:`_with_default_port`, H-review P1-6).
    """
    normalized = _normalize_proxy_url(value)
    scheme = urllib.parse.urlsplit(normalized).scheme.lower()
    if scheme != "http":
        raise ValueError(
            f"unsupported proxy scheme '{scheme}' in '{redact_url(normalized)}': "
            "only http:// proxies are supported"
        )
    check_strict_port(normalized)
    return _with_default_port(normalized, 80)


def _env_first(env: Mapping[str, str], names: tuple[str, ...]) -> str | None:
    for name in names:
        value = env.get(name)
        if value:
            return value
    return None


def _is_ip_address(text: str) -> bool:
    try:
        ipaddress.ip_address(text)
    except ValueError:
        return False
    return True


def _host_matches_entry(host_lower: str, entry: str) -> bool:
    """*host_lower* matches a bare (no-port) ``no_proxy`` *entry*: once
    *host_lower* is itself an IP address, only an exact match counts --
    never a domain suffix (H-review P2-13 — ``0.0.1`` must never bypass
    ``127.0.0.1`` just because the text happens to end the same way a
    real domain suffix would); a named host matches *host_lower*
    exactly or as a domain suffix (a leading dot optional).
    """
    if _is_ip_address(host_lower):
        return host_lower == entry
    return host_lower == entry or host_lower.endswith(f".{entry}")


def _no_proxy_bypasses(no_proxy: str, host: str, port: int | None) -> bool:
    if no_proxy.strip() == "*":
        return True
    host_lower = host.lower()
    for raw_entry in no_proxy.split(","):
        entry = raw_entry.strip().lower()
        if not entry:
            continue
        if _is_ip_address(entry) or _is_ip_address(host_lower):
            # A bare IP (v4, or v6 with no brackets) is never split for
            # a port -- its own colons are address separators, not a
            # port delimiter (an IPv6 entry bracketed *with* a port,
            # `[::1]:8080`, isn't supported either way); once *host* is
            # an IP, only an exact match counts even against a non-IP
            # (malformed) entry text.
            if host_lower == entry:
                return True
            continue
        # H-review P2-13: a `host:port` entry bypasses only that host on
        # that exact port -- never any other port the same host is
        # reached on.
        entry_host, colon, entry_port_text = entry.rpartition(":")
        if colon and entry_host and entry_port_text.isdigit():
            if (
                port is not None
                and int(entry_port_text) == port
                and _host_matches_entry(host_lower, entry_host.lstrip("."))
            ):
                return True
            continue
        if _host_matches_entry(host_lower, entry.lstrip(".")):
            return True
    return False


def resolve_proxy(
    scheme: str, host: str, env: Mapping[str, str], *, port: int | None = None
) -> str | None:
    """The proxy URL for *scheme*/*host*(:*port*) under H1's rule,
    normalized and checked (:func:`check_proxy_url`), or ``None`` when no
    proxy applies — no proxy is configured for *scheme*, or
    ``no_proxy``/``NO_PROXY`` bypasses *host*. *port* only ever matters
    for a ``host:port`` ``no_proxy`` entry (H-review P2-13); omit it (or
    pass ``None``) where the caller has no port to give, which simply
    means no such entry can ever match. *env* is an explicit snapshot,
    never read from the real process environment by this function.
    """
    no_proxy = _env_first(env, ("no_proxy", "NO_PROXY")) or ""
    if _no_proxy_bypasses(no_proxy, host, port):
        return None
    if scheme == "http":
        names: tuple[str, ...] = ("http_proxy",)
    elif scheme == "https":
        names = ("https_proxy", "HTTPS_PROXY")
    else:
        names = ()
    value = _env_first(env, names) or _env_first(env, ("all_proxy", "ALL_PROXY"))
    if not value:
        return None
    return check_proxy_url(value)


@dataclass(frozen=True)
class ProxyAuth:
    """The ``host:port`` and, when the proxy URL carried userinfo, the
    ``Basic`` credentials for it — built from :func:`resolve_proxy`'s
    result by a caller that needs the pieces apart (the ``http`` tool's
    ``Request.set_proxy``; curl never needs this, since it parses the URL
    itself)."""

    hostport: str
    basic_auth: str | None


def split_proxy_url(proxy_url: str) -> ProxyAuth:
    """*proxy_url* (already normalized/checked by :func:`resolve_proxy`)
    as its ``host:port`` and, when present, a base64 ``user:password``
    pair for a ``Proxy-authorization: Basic`` header."""
    parts = urllib.parse.urlsplit(proxy_url)
    hostport = parts.netloc.rpartition("@")[2]
    basic_auth = None
    if parts.username is not None:
        user = urllib.parse.unquote(parts.username)
        password = urllib.parse.unquote(parts.password or "")
        basic_auth = base64.b64encode(f"{user}:{password}".encode()).decode("ascii")
    return ProxyAuth(hostport=hostport, basic_auth=basic_auth)


def apply_request_proxy(req: urllib.request.Request) -> None:
    """Applies H1's proxy rule to *req* in place, the way ``urllib.request.
    ProxyHandler.proxy_open`` would -- minus its own, differently-
    conventioned environment read and ``no_proxy`` bypass, which
    :func:`resolve_proxy` already decides. Shared by every surface that
    issues a request through ``urllib.request`` directly (the ``http``
    tool's own :class:`_RedirectPolicy`, which also needs this on every
    redirect hop; cyberdiner and the ollama ``check()`` probe, which call
    this once per request through :func:`open_with_proxy_rule`). Reads
    the real process environment (``os.environ``) -- the one function in
    this module that does, since it has no caller that needs a synthetic
    one for a test; every proxy *decision* underneath it
    (:func:`resolve_proxy`, :func:`check_proxy_url`) still takes an
    explicit mapping.
    """
    split = urllib.parse.urlsplit(req.full_url)
    proxy_url = resolve_proxy(req.type, split.hostname or "", os.environ, port=split.port)
    if proxy_url is None:
        return
    proxy = split_proxy_url(proxy_url)
    if proxy.basic_auth:
        req.add_header("Proxy-authorization", f"Basic {proxy.basic_auth}")
    req.set_proxy(proxy.hostport, "http")


def open_with_proxy_rule(req: urllib.request.Request, *, timeout: float) -> Any:
    """Issues *req* with :func:`apply_request_proxy` applied and urllib's
    own default proxy-reading convention disabled -- the shared opener a
    surface that doesn't need its own :class:`urllib.request.
    HTTPRedirectHandler` (cyberdiner, the ollama ``check()`` probe) uses
    instead of bare ``urlopen``, which would otherwise let urllib decide
    the proxy under its own, different convention (issue #778 step 7,
    H-review P1-3/P1-4).
    """
    apply_request_proxy(req)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    return opener.open(req, timeout=timeout)
