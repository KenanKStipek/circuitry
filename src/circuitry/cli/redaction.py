"""Defense-in-depth redaction of credential-like values before serialization.

This is intentionally a deny-list, not a guarantee. It targets the most common
shapes of accidental leaks (api_key fields, base_url userinfo, JWT-shaped
strings) so that artifacts written to disk or printed to the console
(`runtime.effective_settings`, `last-run.json`, `--out`, `--json`,
`--live-state`) do not echo back secrets the user passed in. For real secret
hygiene the recommendation in `docs/threat-model.md` still stands: pass
credentials via environment variables, never `-e`.
"""

from __future__ import annotations

import re
from typing import Any
from urllib.parse import urlsplit, urlunsplit

from .. import http_conventions

REDACTED = "***REDACTED***"

#: Keys whose string VALUE gets the URL redaction rule
#: (:func:`http_conventions.redact_url`), regardless of its shape --
#: Circuitry's own literal names, plus any key ending in ``_url`` or
#: ``_proxy`` (which already covers the proxy environment variables:
#: ``http_proxy``, ``https_proxy``, ``all_proxy``, ``no_proxy``, either
#: case). Matched case-insensitively.
_URL_KEY_NAMES = frozenset({"url", "base_url", "api_base", "endpoint", "expo_url", "proxy"})

# Keys whose VALUES should be redacted regardless of content.
# Match is case-insensitive against the final segment of dotted/snake/kebab keys.
_SENSITIVE_KEY_RE = re.compile(
    r"(?ix)^"
    r"(.*[_\-\.])?"
    r"(api[_\-]?key|access[_\-]?key|secret[_\-]?key"
    r"|auth[_\-]?token|access[_\-]?token|bearer[_\-]?token|id[_\-]?token"
    r"|refresh[_\-]?token|session[_\-]?token|csrf[_\-]?token"
    r"|authorization|password|passphrase|client[_\-]?secret"
    r"|set[_\-]?cookie|cookie"
    r"|secret|token|credentials?)$"
)

# Standalone JWT (three base64url segments separated by dots). Every real
# JWT header starts with "eyJ" (base64url of the JSON header's leading
# '{"'), so requiring it keeps plain dotted names (www.example.com,
# a.tar.gz, 1.2.3) from matching.
_JWT_RE = re.compile(r"^eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+$")

# Common API-key shapes — long base64url-ish or hex strings, often with a
# vendor prefix. Conservative: require at least 32 chars to avoid false
# positives on short identifiers.
_KEYISH_RE = re.compile(
    r"(?:^sk-[A-Za-z0-9_\-]{20,}$)"
    r"|(?:^xox[abposr]-[A-Za-z0-9\-]{20,}$)"
    r"|(?:^ghp_[A-Za-z0-9]{30,}$)"
    r"|(?:^Bearer\s+[A-Za-z0-9_\-\.=]{16,}$)"
)


def _redact_url(value: str) -> str:
    """If a URL contains userinfo (user[:pass]@host), strip it -- exactly
    `main`'s own, unmodified redaction of an ordinary string: both
    ``scheme`` and ``netloc`` (so bare prose with a stray ``://`` or
    ``@`` never matches) and the ``@`` inside ``netloc`` itself (so a
    plain email address, or a URL with no userinfo, never matches).
    """
    try:
        parts = urlsplit(value)
    except ValueError:
        return value
    if not parts.scheme or not parts.netloc:
        return value
    if "@" not in parts.netloc:
        return value
    _, _, host = parts.netloc.rpartition("@")
    redacted_netloc = f"{REDACTED}@{host}"
    return urlunsplit((parts.scheme, redacted_netloc, parts.path, parts.query, parts.fragment))


def _is_token_shaped(value: str) -> bool:
    """Whether *value* is a JWT or API-key-shaped string -- `main`'s
    own ordinary-string rule, applied first even to a string under a
    URL key (:func:`_is_url_key`): a token-shaped value redacts
    wholesale regardless of the key it's under, same as it always was
    on `main` -- a URL key's own value is never sent *only* through
    :func:`http_conventions.redact_url`.
    """
    return bool(_JWT_RE.match(value) or _KEYISH_RE.match(value))


def _redact_string(value: str) -> str:
    """An ordinary string, redacted exactly as `main` redacts it: a JWT
    or API-key-shaped string wholesale, a URL that carries both
    ``://`` and ``@`` through :func:`_redact_url`, anything else
    unchanged. The URL rule in :func:`http_conventions.redact_url`
    never applies here -- only to a string under a URL key
    (:func:`_is_url_key`), handled by :func:`redact` itself.
    """
    if _is_token_shaped(value):
        return REDACTED
    if "://" in value and "@" in value:
        return _redact_url(value)
    return value


def _is_sensitive_key(key: str) -> bool:
    return bool(_SENSITIVE_KEY_RE.match(key))


def _is_url_key(key: str) -> bool:
    """Whether *key* names a value :func:`http_conventions.redact_url`
    applies to, regardless of its content.
    """
    lower = key.lower()
    return lower in _URL_KEY_NAMES or lower.endswith(("_url", "_proxy"))


def _redact_adapter_env(adapters: Any) -> Any:
    """``runtime.adapters`` with every ``<adapter>.env`` value replaced.

    ``env`` is the environment an adapter alias gives its CLI (a config
    directory, a profile name, ...): whatever it holds, none of it belongs in
    an artifact. The variable names stay, so a reader can see what is set. An
    ``env`` that is not a mapping (a config error preflight rejects, but the
    snapshot is written first) is replaced whole.
    """
    if not isinstance(adapters, dict):
        return redact(adapters)
    out: dict[Any, Any] = {}
    for name, entry in adapters.items():
        if isinstance(entry, dict) and "env" in entry:
            env = entry["env"]
            blanked = dict.fromkeys(env, REDACTED) if isinstance(env, dict) else REDACTED
            out[name] = redact({**entry, "env": blanked})
        else:
            out[name] = redact(entry)
    return out


def redact(value: Any) -> Any:
    """Recursively redact credential-like fields.

    - Dict values are redacted when the key matches the sensitive-name pattern.
    - Every value of an ``env`` mapping under an ``adapters`` key is redacted
      (``runtime.adapters.<alias>.env``); an ``env`` that is not a mapping is
      replaced whole.
    - A string value under a URL key (``url``, ``base_url``, ``api_base``,
      ``endpoint``, ``expo_url``, ``proxy``, any key ending in ``_url`` or
      ``_proxy``, at any depth) gets the URL rule
      (:func:`http_conventions.redact_url`), regardless of its shape.
    - Any other string value is redacted when it looks like a JWT, an API
      key, or a URL carrying both ``://`` and ``@`` -- exactly `main`'s own
      redaction of an ordinary string.
    - Lists and nested dicts are walked.
    - Other scalar types pass through unchanged.

    Returns a new structure; the input is not mutated.
    """
    if isinstance(value, dict):
        out: dict[Any, Any] = {}
        for k, v in value.items():
            if isinstance(k, str) and _is_sensitive_key(k):
                out[k] = REDACTED
            elif k == "adapters":
                out[k] = _redact_adapter_env(v)
            elif isinstance(k, str) and _is_url_key(k) and isinstance(v, str):
                out[k] = REDACTED if _is_token_shaped(v) else http_conventions.redact_url(v, REDACTED)
            else:
                out[k] = redact(v)
        return out
    if isinstance(value, list):
        return [redact(item) for item in value]
    if isinstance(value, tuple):
        return tuple(redact(item) for item in value)
    if isinstance(value, str):
        return _redact_string(value)
    return value


def redact_env_pairs(pairs: list[str] | None) -> list[str] | None:
    """Redact a list of `KEY=VALUE` strings (the `-e` flag shape).

    The KEY is preserved (so the user can see what was supplied at replay
    time), the VALUE is replaced with the redaction sentinel when the key
    matches a sensitive-name pattern. Returns `None` when given `None`.
    """
    if pairs is None:
        return None
    out: list[str] = []
    for pair in pairs:
        if "=" not in pair:
            out.append(pair)
            continue
        key, value = pair.split("=", 1)
        if _is_sensitive_key(key):
            out.append(f"{key}={REDACTED}")
        elif _is_url_key(key):
            masked = REDACTED if _is_token_shaped(value) else http_conventions.redact_url(value, REDACTED)
            out.append(f"{key}={masked}")
        else:
            # Even non-sensitive keys can carry credential-shaped values
            # (e.g. `-e my_var=sk-abc...`). Apply string-level checks too.
            out.append(f"{key}={_redact_string(value)}")
    return out
