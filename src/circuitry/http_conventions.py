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
import string
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Any

#: The only two schemes a request URL (or an adapter's `base_url`) may use.
ALLOWED_SCHEMES = ("http", "https")

_PORT_RE = re.compile(r"^[0-9]+$")

#: Any ASCII control character (0x00-0x1F, 0x7F) or ASCII whitespace
#: (space is 0x20, already inside that first range) -- a URL or proxy
#: value containing one is refused before any I/O, on both engines, with
#: one shared message (issue #778).
_CONTROL_OR_WHITESPACE_RE = re.compile(r"[\x00-\x20\x7f]")

#: Query-string keys that commonly carry a credential (search APIs, some
#: webhook/trigger URLs). Matched case-insensitively against the whole
#: key, so ``api_key``, ``apiKey``, ``access_token`` and ``X-Auth-Token``
#: all match.
_CREDENTIAL_QUERY_KEY_RE = re.compile(r"(key|token|secret|auth|password|pwd)", re.IGNORECASE)

#: `urllib.parse`'s own scheme-character set (RFC 3986's `ALPHA *(
#: ALPHA / DIGIT / "+" / "-" / "." )`), used only by
#: :func:`_urlsplit_lenient`'s own scheme detection.
_SCHEME_CHARS = frozenset(
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-."
)


def _urlsplit_lenient(url: str) -> urllib.parse.SplitResult:
    """*url* split the same way :func:`urllib.parse.urlsplit` does, but
    without its own IPv6-bracket validation -- CPython's own
    ``_check_bracketed_netloc`` raises ``ValueError`` ("Invalid IPv6
    URL") for something like ``[::1]x``, where the strict-port check
    means to refuse with the shared "invalid port" text instead (issue
    #778). Used only as :func:`_urlsplit`'s fallback when the real
    ``urlsplit`` itself raises; every other input still goes through
    CPython's own, stricter parser directly.
    """
    scheme = ""
    rest = url
    colon = rest.find(":")
    if (
        colon > 0
        and rest[:1].isascii()
        and rest[:1].isalpha()
        and all(c in _SCHEME_CHARS for c in rest[:colon])
    ):
        scheme, rest = rest[:colon].lower(), rest[colon + 1 :]
    netloc = ""
    if rest[:2] == "//":
        delim = len(rest)
        for sep in "/?#":
            idx = rest.find(sep, 2)
            if idx != -1:
                delim = min(delim, idx)
        netloc, rest = rest[2:delim], rest[delim:]
    fragment = ""
    if "#" in rest:
        rest, _, fragment = rest.partition("#")
    query = ""
    if "?" in rest:
        rest, _, query = rest.partition("?")
    return urllib.parse.SplitResult(scheme, netloc, rest, query, fragment)


def _urlsplit(url: str) -> urllib.parse.SplitResult:
    """:func:`urllib.parse.urlsplit`, falling back to
    :func:`_urlsplit_lenient` when CPython's own IPv6-bracket check
    raises (issue #778) -- every refusal in this module names *url*
    with the shared text, never CPython's own bare exception.
    """
    try:
        return urllib.parse.urlsplit(url)
    except ValueError:
        return _urlsplit_lenient(url)


def _mask_credential_query_params(url: str) -> str:
    """*url* with the value of any query-string parameter whose key looks
    like a credential masked, e.g. ``https://api.example.com/search?key=
    secret`` becomes ``...?key=%2A%2A%2A``."""
    parsed = _urlsplit(url)
    if not parsed.query:
        return url
    pairs = urllib.parse.parse_qsl(parsed.query, keep_blank_values=True)
    masked = [(k, "***" if _CREDENTIAL_QUERY_KEY_RE.search(k) else v) for k, v in pairs]
    new_query = urllib.parse.urlencode(masked)
    return urllib.parse.urlunsplit(parsed._replace(query=new_query))


def strip_request_url_userinfo(url: str) -> str:
    """*url*'s userinfo removed from its authority -- the LAST ``@``
    inside :func:`urllib.parse.urlsplit`'s own ``netloc``, not a ``/``,
    ``?``, ``#`` or whitespace boundary: this always receives one whole
    URL, never prose with one embedded somewhere, so building the
    redaction from the parsed authority directly -- instead of a regex
    heuristic -- means a password containing any of those (none of
    which end ``urlsplit``'s own ``netloc`` the way ``/`` does) still
    comes out fully removed (issue #778).

    Also used, directly (not only through :func:`redact_request_url`),
    by every surface that builds a plain :class:`urllib.request.Request` (the
    ``http`` tool, cyberdiner): urllib keeps a URL's userinfo verbatim
    in ``Request.host`` and never turns it into an ``Authorization``
    header the way electricity's own wire building does, so a userinfo
    containing a colon but no port (``http://user:pass@host/``) reaches
    ``http.client``'s own host/port split unstripped and raises its
    confusing, credential-bearing ``InvalidURL("nonnumeric port: ...")``
    -- stripped first, it never sees the userinfo at all (issue #778).
    """
    parsed = _urlsplit(url)
    if "@" not in parsed.netloc:
        return url
    netloc = parsed.netloc.rpartition("@")[2]
    return urllib.parse.urlunsplit(parsed._replace(netloc=netloc))


def _redact_url(url: str) -> str:
    """*url* with userinfo stripped, then any credential-like query
    parameter masked — the two masks every refusal message that names a
    request URL (never a proxy value, see :func:`redact_proxy_value`)
    applies before naming it. Never called directly outside this module
    (:func:`redact_request_url` is the one public redactor for a request
    URL); kept for the tests that exercise it in isolation."""
    return strip_request_url_userinfo(_mask_credential_query_params(url))


def redact_proxy_value(value: str) -> str:
    """*value* (a proxy URL or value, normalized or not) with its
    userinfo removed, for a refusal message. A proxy value has no
    meaningful path, so -- unlike :func:`_redact_url` -- this never stops
    at a ``/``, ``?``, ``#`` or whitespace: only the LAST ``@`` in the
    *whole* value (after its ``scheme://``, when it has one) counts, so
    a password containing any of those still comes out fully removed
    (issue #778).
    """
    scheme_end = value.find("://")
    prefix, tail = (
        (value[: scheme_end + 3], value[scheme_end + 3 :])
        if scheme_end != -1
        else ("", value)
    )
    if "@" not in tail:
        return value
    return prefix + tail.rpartition("@")[2]


def _at_follows_netloc(url: str, netloc: str) -> bool:
    """Whether an ``@`` appears anywhere in *url* after its parsed
    *netloc* -- a password hidden behind a ``/``, ``?`` or ``#`` can
    still smuggle a second, real host past a boundary that stops at
    the first one (issue #778). True regardless of whether *netloc*
    already carries userinfo of its own: an email address used as a
    username (``https://me@corp.com:pa/ss@api.example.com/v1``) still
    leaves a credential fragment exposed if this were ever gated on
    *netloc* having no ``@`` of its own.
    """
    scheme_sep = url.find("://")
    rest = url[scheme_sep + 3 + len(netloc) :] if scheme_sep != -1 else ""
    return "@" in rest


def redact_request_url(url: str) -> str:
    """*url*, redacted for any refusal, warning, event, state, hint or
    ``host:`` text that names a request URL -- the one function every
    such caller uses, never :func:`_redact_url` or :func:`redact_proxy_value`
    directly: the whole value is cut at the LAST ``@`` (never just the
    authority's own userinfo) when *url*'s parsed ``netloc`` is empty
    (a userinfo-bearing value with no scheme at all, or one whose
    scheme has no ``//`` authority of its own even though the literal
    text ``://`` appears somewhere later in it, e.g. in a query value --
    neither leaves :func:`urlsplit` any authority for
    :func:`strip_request_url_userinfo` to find anything in) or
    :func:`_at_follows_netloc` is true (a password hidden behind a
    ``/``, ``?`` or ``#`` can still smuggle a second, real host past the
    parsed authority); :func:`strip_request_url_userinfo` otherwise.
    Either way, the cut runs *before* any credential-like query
    parameter is masked: masking first re-encodes a query string's own
    ``@`` as ``%40``, so a cut that ran afterward would no longer find
    it (issue #778). Shared by every surface that names a request URL
    after parsing it: :func:`check_url`, :func:`check_strict_port`,
    :func:`curl_support.redact_host_url`,
    :func:`curl_support.curl_failure_message`, and every adapter error
    that names a request URL after I/O.
    """
    netloc = _urlsplit(url).netloc
    if not netloc:
        cut = url.rpartition("@")[2]
    elif _at_follows_netloc(url, netloc):
        cut = redact_proxy_value(url)
    else:
        cut = strip_request_url_userinfo(url)
    return _mask_credential_query_params(cut)


def _host_port_text(netloc: str) -> str | None:
    """The raw port text in *netloc* (userinfo already stripped by the
    caller): ``None`` with no ``:`` separator at all (or none after an
    IPv6 literal's own ``]``); otherwise the (possibly empty) text
    after it. A bracketed IPv6 host's own colons are never split -- and
    anything after its ``]`` that doesn't even start with ``:``
    (``[::1]x``) is not "no port" either: it's returned as-is, so the
    strict-port digit check below refuses it with the shared text
    rather than silently accepting it (issue #778).
    """
    if netloc.startswith("["):
        end = netloc.find("]")
        rest = netloc[end + 1 :] if end != -1 else ""
    elif ":" in netloc:
        rest = ":" + netloc.partition(":")[2]
    else:
        return None
    if not rest:
        return None
    return rest.removeprefix(":")


def _port_in_range(port_text: str) -> bool:
    return bool(_PORT_RE.fullmatch(port_text)) and 1 <= int(port_text) <= 65535


def check_strict_port(url: str) -> None:
    """Refuses *url* before any I/O when its port isn't strictly valid:
    after the host (or after ``]`` for IPv6) comes ``:`` and one or
    more ASCII digits, 1-65535. An empty port, or none at all, means the
    scheme's default and is never refused.

    When an ``@`` appears anywhere in *url* after its parsed authority
    (:func:`_at_follows_netloc`) -- e.g. ``https://user:pa/ss@api.example.
    com/v1`` (the authority ends at the first ``/``, so ``urlsplit`` reads
    ``pa`` as a port attempt, not as half of a password), or
    ``https://me@corp.com:pa/ss@api.example.com/v1`` (the authority's own
    userinfo is itself an email address, ``me@corp.com``, and ``pa`` is
    still a port attempt, not half of a password) -- the "port" text is
    really a credential fragment: the refusal never echoes it, and names
    *url* through :func:`redact_request_url` -- everything up to the
    LAST ``@`` in the whole value cut away instead of the usual
    userinfo-stripped form, whether or not the authority's own userinfo
    already has an ``@`` of its own -- rather than the ordinary
    userinfo-stripped-and-port-named text (issue #778).
    """
    netloc = _urlsplit(url).netloc
    host_port = netloc.rpartition("@")[2]
    port_text = _host_port_text(host_port)
    if not port_text:
        return
    if _port_in_range(port_text):
        return
    redacted = redact_request_url(url)
    if _at_follows_netloc(url, netloc):
        raise ValueError(f"invalid port in URL '{redacted}'")
    raise ValueError(f"invalid port in URL '{redacted}': '{port_text}'")


def _check_no_control_or_whitespace(value: str, *, redact: Callable[[str], str]) -> None:
    """Refuses *value* before any I/O when it contains any ASCII control
    character or whitespace (issue #778) -- one shared message on both
    engines, never a surface-specific one. *redact* is whichever of
    :func:`redact_request_url`/:func:`redact_proxy_value` fits *value*'s
    own shape.
    """
    if _CONTROL_OR_WHITESPACE_RE.search(value):
        raise ValueError(
            f"URL contains a control character or whitespace: '{redact(value)}'"
        )


def check_url(url: str, *, allowed_schemes: tuple[str, ...] = ALLOWED_SCHEMES) -> None:
    """Refuses *url* before any I/O: its scheme must be one of
    *allowed_schemes*, and its port must pass :func:`check_strict_port`.
    """
    _check_no_control_or_whitespace(url, redact=redact_request_url)
    scheme = _urlsplit(url).scheme.lower()
    if scheme not in allowed_schemes:
        allowed = " or ".join(allowed_schemes)
        raise ValueError(
            f"unsupported URL scheme '{scheme}' in '{redact_request_url(url)}': "
            f"only {allowed} is supported"
        )
    check_strict_port(url)


def _normalize_proxy_url(value: str) -> str:
    """*value* with a scheme prefixed (``http://``) when it has none at
    all — the proxy rule's "a proxy URL with no scheme means http://"
    (issue #778)."""
    return value if "://" in value else f"http://{value}"


def _proxy_authority(normalized: str) -> str:
    """The authority text of *normalized* (a proxy value that already
    has a ``scheme://``): bounded at the first ``/``, ``?`` or ``#``
    after it -- the same boundary curl, ``http.client`` and a raw
    socket all use when they read the value back out, so
    ``http://proxy.corp:3128/`` names the authority ``proxy.corp:3128``,
    never the whole remainder (issue #778). Refuses *normalized* when an
    ``@`` appears in the part *after* that boundary: a password hidden
    behind a ``/``, ``?`` or ``#`` to smuggle a second, *real* host past
    whichever of those functions stops at the first one would otherwise
    reach them unchecked.
    """
    after_scheme = normalized.split("://", 1)[1]
    boundary = len(after_scheme)
    for sep in "/?#":
        idx = after_scheme.find(sep)
        if idx != -1 and idx < boundary:
            boundary = idx
    authority, tail = after_scheme[:boundary], after_scheme[boundary:]
    if "@" in tail:
        raise ValueError(f"invalid proxy URL '{redact_proxy_value(normalized)}'")
    return authority


def _proxy_host_port(normalized: str) -> str:
    """The host[:port] text of *normalized* (a proxy value that already
    has a ``scheme://``): :func:`_proxy_authority`, split at the LAST
    ``@`` -- a proxy value's userinfo may itself contain one (issue
    #778), but never past the authority's own boundary any more.
    """
    return _proxy_authority(normalized).rpartition("@")[2]


def _check_strict_port_for_proxy(normalized: str) -> None:
    """Like :func:`check_strict_port`, but for a proxy value: its
    host[:port] text comes from :func:`_proxy_host_port`, not
    ``urlsplit``'s own netloc, and any refusal names *normalized* through
    :func:`redact_proxy_value`.
    """
    port_text = _host_port_text(_proxy_host_port(normalized))
    if not port_text:
        return
    if _port_in_range(port_text):
        return
    raise ValueError(f"invalid port in URL '{redact_proxy_value(normalized)}': '{port_text}'")


def _with_default_port(url: str, default_port: int) -> str:
    """*url* (a proxy value already checked by
    :func:`_check_strict_port_for_proxy`) with ``:<default_port>``
    written into its host[:port] text when it names no port of its own,
    not even an empty one (an absent port means the scheme's own
    default). Every surface that reads this normalized proxy URL back
    out (curl's ``--config``, the ``http`` tool's ``Request.set_proxy``)
    must see the real default explicitly, since curl's *own* default for
    a schemeless/portless proxy is 1080, its historic SOCKS-proxy
    convention — never this module's own 80 (issue #778). The authority
    (issue #778) is no longer always a suffix of *url* -- a trailing
    ``/``, ``?`` or ``#`` (and whatever follows it) can come after it --
    so the default port is spliced in right after the authority ends,
    never appended to the end of *url* itself.
    """
    authority = _proxy_authority(url)
    host_port = authority.rpartition("@")[2]
    if _host_port_text(host_port):
        return url
    authority_end = url.find("://") + 3 + len(authority)
    separator = "" if host_port.endswith(":") else ":"
    return f"{url[:authority_end]}{separator}{default_port}{url[authority_end:]}"


def check_proxy_url(value: str) -> str:
    """*value*, normalized, with its scheme and port checked before
    any I/O: only an ``http`` proxy scheme is supported, and its port
    follows :func:`_check_strict_port_for_proxy`. The returned URL always
    names a port explicitly (:func:`_with_default_port`). Normalized
    *before* any check runs, including the control-character one, so a
    schemeless value (which has no ``://`` of its own for
    :func:`redact_proxy_value` to find) is still named safely in every
    refusal this raises (issue #778).
    """
    normalized = _normalize_proxy_url(value)
    _check_no_control_or_whitespace(normalized, redact=redact_proxy_value)
    scheme = _urlsplit(normalized).scheme.lower()
    if scheme != "http":
        raise ValueError(
            f"unsupported proxy scheme '{scheme}' in '{redact_proxy_value(normalized)}': "
            "only http:// proxies are supported"
        )
    _check_strict_port_for_proxy(normalized)
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
    never a domain suffix (issue #778 — ``0.0.1`` must never bypass
    ``127.0.0.1`` just because the text happens to end the same way a
    real domain suffix would); a named host matches *host_lower*
    exactly or as a domain suffix (a leading dot optional).
    """
    if _is_ip_address(host_lower):
        return host_lower == entry
    return host_lower == entry or host_lower.endswith(f".{entry}")


#: The scheme's own default port -- used only to decide whether a
#: `host:port` no_proxy entry matches a URL that named no port of its
#: own: `example.com:443` still matches `https://example.com/`, whose
#: implicit port is 443 (issue #778).
_SCHEME_DEFAULT_PORTS = {"http": 80, "https": 443}


def _no_proxy_bypasses(no_proxy: str, scheme: str, host: str, port: int | None) -> bool:
    if no_proxy.strip() == "*":
        return True
    effective_port = port if port is not None else _SCHEME_DEFAULT_PORTS.get(scheme)
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
        # A `host:port` entry bypasses only that host on that exact
        # port -- never any other port the same host is reached on
        # (issue #778). The port text is ASCII-only (`isdigit()` alone
        # accepts any Unicode decimal digit, which `int()` then
        # rejects anyway -- never a match either way, just a plain one
        # instead of a crash).
        entry_host, colon, entry_port_text = entry.rpartition(":")
        if colon and entry_host and entry_port_text.isascii() and entry_port_text.isdigit():
            if (
                effective_port is not None
                and int(entry_port_text) == effective_port
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
    """The proxy URL for *scheme*/*host*(:*port*) under the proxy rule,
    normalized and checked (:func:`check_proxy_url`), or ``None`` when no
    proxy applies — no proxy is configured for *scheme*, or
    ``no_proxy``/``NO_PROXY`` bypasses *host*. *port* only ever matters
    for a ``host:port`` ``no_proxy`` entry (issue #778); omit it (or
    pass ``None``) where the caller's own URL named no port of its own
    -- a ``host:port`` entry still matches *scheme*'s own implicit
    default port then (``example.com:443`` matches a portless
    ``https://example.com/``), never "no such entry can ever match".
    *env* is an explicit snapshot, never read from the real process
    environment by this function.
    """
    no_proxy = _env_first(env, ("no_proxy", "NO_PROXY")) or ""
    if _no_proxy_bypasses(no_proxy, scheme, host, port):
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
    """Applies the proxy rule to *req* in place, the way ``urllib.request.
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


class _OpenWithProxyRuleRedirectHandler(urllib.request.HTTPRedirectHandler):
    """The redirect handling :func:`open_with_proxy_rule`'s own opener
    needs instead of urllib's bare default: that default copies a
    request's ``Proxy-authorization`` header onto the next hop
    unchanged, resolves no proxy of its own for it (the opener's empty
    ``ProxyHandler({})`` never reconsiders), and lets an ``ftp:``
    redirect target through its own narrower prefilter -- so every
    redirect hop would otherwise go direct, carrying the previous hop's
    proxy credential to a target that was never meant to see it (issue
    #778). :func:`check_url` runs only once the method and loop
    decisions below have already committed to following this hop,
    inside :meth:`http_error_302` itself, after its own loop-detection
    block -- matching ``plugins/http.py``'s own ``_RedirectPolicy``.
    """

    def http_error_302(
        self, req: urllib.request.Request, fp: Any, code: int, msg: str, headers: Any
    ) -> Any:
        if "location" in headers:
            newurl = headers["location"]
        elif "uri" in headers:
            newurl = headers["uri"]
        else:
            return None

        urlparts: Any = urllib.parse.urlparse(newurl)
        if not urlparts.path and urlparts.netloc:
            urlparts = list(urlparts)
            urlparts[2] = "/"
        newurl = urllib.parse.urlunparse(urlparts)

        newurl = urllib.parse.quote(newurl, encoding="iso-8859-1", safe=string.punctuation)
        newurl = urllib.parse.urljoin(req.full_url, newurl)
        # Stripped before urllib's own `redirect_request` ever builds a
        # `Request` from it (issue #778, matching `plugins/http.py`'s
        # own `_RedirectPolicy`): otherwise a userinfo with no port
        # (`http://user:pass@host/`) reaches `http.client`'s host/port
        # split unstripped and raises its own credential-bearing
        # `InvalidURL("nonnumeric port: ...")` instead.
        newurl = strip_request_url_userinfo(newurl)

        new = self.redirect_request(req, fp, code, msg, headers, newurl)
        if new is None:
            return None

        if hasattr(req, "redirect_dict"):
            visited = new.redirect_dict = req.redirect_dict  # type: ignore[attr-defined]
            if (
                visited.get(newurl, 0) >= self.max_repeats
                or len(visited) >= self.max_redirections
            ):
                raise urllib.error.HTTPError(req.full_url, code, self.inf_msg + msg, headers, fp)
        else:
            visited = new.redirect_dict = req.redirect_dict = {}  # type: ignore[attr-defined]
        visited[newurl] = visited.get(newurl, 0) + 1

        check_url(newurl)
        new.remove_header("Proxy-authorization")
        apply_request_proxy(new)

        fp.read()
        fp.close()
        return self.parent.open(new, timeout=req.timeout)

    http_error_301 = http_error_303 = http_error_307 = http_error_308 = http_error_302


def open_with_proxy_rule(req: urllib.request.Request, *, timeout: float) -> Any:
    """Issues *req* with :func:`apply_request_proxy` applied, urllib's own
    default proxy-reading convention disabled, and
    :class:`_OpenWithProxyRuleRedirectHandler` reapplying the proxy rule
    on every redirect hop -- the shared opener a surface that doesn't
    need its own :class:`urllib.request.HTTPRedirectHandler` (cyberdiner,
    the ollama ``list_models`` probe) uses instead of bare ``urlopen``,
    which would otherwise let urllib decide the proxy under its own,
    different convention on the first hop, and carry a proxy credential
    straight to a redirect target with no proxy of its own on every hop
    after that (issue #778).
    """
    apply_request_proxy(req)
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), _OpenWithProxyRuleRedirectHandler()
    )
    return opener.open(req, timeout=timeout)
