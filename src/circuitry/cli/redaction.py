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

REDACTED = "***REDACTED***"

# Keys whose VALUES should be redacted regardless of content.
# Match is case-insensitive against the final segment of dotted/snake/kebab keys.
_SENSITIVE_KEY_RE = re.compile(
    r"(?ix)^"
    r"(.*[_\-\.])?"
    r"(api[_\-]?key|access[_\-]?key|secret[_\-]?key"
    r"|auth[_\-]?token|access[_\-]?token|bearer[_\-]?token|id[_\-]?token"
    r"|refresh[_\-]?token|session[_\-]?token|csrf[_\-]?token"
    r"|authorization|password|passphrase|client[_\-]?secret"
    r"|secret|token|credentials?)$"
)

# Standalone JWT (three base64url segments separated by dots).
_JWT_RE = re.compile(r"^[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+$")

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
    """If a URL contains userinfo (user[:pass]@host), strip it."""
    try:
        parts = urlsplit(value)
    except ValueError:
        return value
    if not parts.scheme or not parts.netloc:
        return value
    if "@" not in parts.netloc:
        return value
    # netloc looks like "user:pass@host:port" — keep only host:port
    _, _, host = parts.netloc.rpartition("@")
    redacted_netloc = f"{REDACTED}@{host}"
    return urlunsplit((parts.scheme, redacted_netloc, parts.path, parts.query, parts.fragment))


def _redact_string(value: str) -> str:
    if _JWT_RE.match(value):
        return REDACTED
    if _KEYISH_RE.match(value):
        return REDACTED
    if "://" in value and "@" in value:
        # Treat as URL with possible userinfo.
        return _redact_url(value)
    return value


def _is_sensitive_key(key: str) -> bool:
    return bool(_SENSITIVE_KEY_RE.match(key))


def redact(value: Any) -> Any:
    """Recursively redact credential-like fields.

    - Dict values are redacted when the key matches the sensitive-name pattern.
    - String values are redacted when they look like a JWT, an API key, or a
      URL with userinfo.
    - Lists and nested dicts are walked.
    - Other scalar types pass through unchanged.

    Returns a new structure; the input is not mutated.
    """
    if isinstance(value, dict):
        out: dict[Any, Any] = {}
        for k, v in value.items():
            if isinstance(k, str) and _is_sensitive_key(k):
                out[k] = REDACTED
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
        else:
            # Even non-sensitive keys can carry credential-shaped values
            # (e.g. `-e my_var=sk-abc...`). Apply string-level checks too.
            out.append(f"{key}={_redact_string(value)}")
    return out
