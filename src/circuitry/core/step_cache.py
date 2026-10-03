"""Per-step result cache for `cache:` on `prompt`/`tool` effects (#270).

Opt-in, off by default. A `prompt`/`tool` effect that sets `cache: true` or
`cache: {ttl: ..., key: ...}` (see `core.compiler._compile_cache`) is looked
up by a content hash of everything that shapes its result once rendered —
see `PromptRuntime._cache_key`/`ToolRuntime._cache_key` for exactly what
goes into each — before it dispatches. A hit skips the dispatch entirely
(no concurrency slot, no retries, no tokens) and reuses the stored value;
a miss dispatches normally and, if it succeeds, stores the result for next
time. Only a result that reached its effect's own success condition is
ever stored — a failure, including one `on_error: continue` absorbed, is
never cached (see each runtime's own success branch).

Storage is one JSON file per key under a private per-user directory
(`cache_dir()`), written atomically and kept at the tightest permissions
available (0700 dir, 0600 file) — a cached value can be exactly as
sensitive as the prompt/tool call that produced it (a model's reply about
private data, a tool's fetched secret), and it must never land in a run's
own `--out`/`--state`/persistence record, only in this private store.

`CIRCUITRY_CACHE_DIR` overrides the location outright (what the test suite's
hermetic fixture uses — see `tests/conftest.py` — so a real developer cache
is never read or written by a test run); otherwise it is
`$XDG_CACHE_HOME/circuitry/steps` or `~/.cache/circuitry/steps`.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

__all__ = [
    "CACHE_FORMAT_VERSION",
    "NO_CACHE_RUNTIME_CONFIG_KEY",
    "CacheDef",
    "CacheLookup",
    "StepCache",
    "cache_dir",
    "compute_cache_key",
    "parse_ttl",
]

#: Bumped whenever the key material (what goes into the hash) or the stored
#: record's shape changes, so an old cache from a prior version of this
#: module is never misread as a hit for a differently-keyed entry.
CACHE_FORMAT_VERSION = 1

#: The ``runtime_config`` dict key `cof run --no-cache` sets — read by both
#: ``PromptRuntime``/``ToolRuntime`` to skip reading *and* writing the cache
#: for the whole run, the same way ``core.concurrency.RUNTIME_CONFIG_KEY``
#: threads the run's concurrency limiter through every nested runtime.
NO_CACHE_RUNTIME_CONFIG_KEY = "_no_cache"

#: Overrides the cache directory outright — see the module docstring.
_CACHE_DIR_ENV = "CIRCUITRY_CACHE_DIR"

_TTL_PATTERN = re.compile(r"^(\d+)\s*([smhd])$", re.IGNORECASE)
_TTL_UNIT_SECONDS = {"s": 1, "m": 60, "h": 3600, "d": 86400}


def parse_ttl(raw: Any, *, label: str) -> float:
    """A ``ttl:`` value (``"7d"``, ``"12h"``, ``"30m"``, ``"45s"``, or a bare
    number of seconds) as seconds. Raises ``ValueError`` naming *label* on
    anything else, including a negative/zero duration."""
    if isinstance(raw, bool):
        raise ValueError(
            f"{label}: 'ttl' must be a duration string (e.g. '7d') or a number "
            f"of seconds, got {raw!r}."
        )
    if isinstance(raw, (int, float)):
        if raw <= 0:
            raise ValueError(f"{label}: 'ttl' must be positive, got {raw!r}.")
        return float(raw)
    if isinstance(raw, str):
        match = _TTL_PATTERN.fullmatch(raw.strip())
        if match is not None:
            value, unit = match.groups()
            seconds = int(value) * _TTL_UNIT_SECONDS[unit.lower()]
            if seconds > 0:
                return float(seconds)
        raise ValueError(
            f"{label}: 'ttl' must look like '7d', '12h', '30m', '45s', or a "
            f"positive number of seconds, got {raw!r}."
        )
    raise ValueError(
        f"{label}: 'ttl' must be a string or number, got {type(raw).__name__}."
    )


@dataclass(frozen=True)
class CacheDef:
    """``cache:`` on a `prompt`/`tool` effect. A bare ``true`` normalizes to
    this with no ttl/salt at compile time (see `core.compiler._compile_cache`).
    """

    #: ``None``: no expiry, a hit is valid however old. Otherwise seconds.
    ttl_seconds: float | None = None
    #: ``cache: {key: ...}`` — an extra salt folded into the hash for when
    #: an input outside the rendered effect itself changes (a file on disk,
    #: model weights swapped in place) and the author wants a fresh entry
    #: without changing anything the key would otherwise see.
    key_salt: str | None = None


def cache_dir() -> Path:
    """The per-user step-cache directory, created at mode 0700 if missing."""
    override = os.environ.get(_CACHE_DIR_ENV)
    if override:
        path = Path(override)
    else:
        base = os.environ.get("XDG_CACHE_HOME")
        root = Path(base) if base else Path.home() / ".cache"
        path = root / "circuitry" / "steps"
    path.mkdir(parents=True, exist_ok=True)
    try:
        os.chmod(path, 0o700)
    except OSError:
        pass
    return path


def compute_cache_key(*, effect_type: str, material: dict[str, Any], salt: str | None) -> str:
    """A stable sha256 hex digest of *material*, scoped by *effect_type* and
    the cache format version, plus an optional caller-supplied *salt*.

    ``material`` must be JSON-serializable (``default=str`` covers the rest,
    e.g. a non-string mapping key already rejected upstream); key order
    never matters — ``json.dumps(..., sort_keys=True)`` normalizes it.
    """
    payload: dict[str, Any] = {
        "cache_format_version": CACHE_FORMAT_VERSION,
        "effect_type": effect_type,
        "material": material,
    }
    if salt:
        payload["salt"] = salt
    encoded = json.dumps(payload, sort_keys=True, default=str, ensure_ascii=True).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


@dataclass(frozen=True)
class CacheLookup:
    """The result of a `StepCache.get` — `hit=False` on a miss, an expired
    entry, or a corrupt/unreadable one; never raises."""

    hit: bool
    value: Any = None
    created_at: str | None = None


class StepCache:
    """Reads/writes cached effect results under `cache_dir()`.

    One file per key, `<cache_dir>/<key>.json`, holding
    `{"created_at": <iso>, "value": <json value>}`. Written atomically
    (tempfile in the same directory, then `os.replace`) at mode 0600, so a
    reader never observes a partially-written file and a cached value —
    which can be sensitive — is never briefly world/group readable.
    """

    def __init__(self, root: Path | None = None) -> None:
        self._root = root if root is not None else cache_dir()

    def _path(self, key: str) -> Path:
        return self._root / f"{key}.json"

    def get(self, key: str, *, ttl_seconds: float | None) -> CacheLookup:
        try:
            raw = self._path(key).read_text(encoding="utf-8")
        except (FileNotFoundError, OSError):
            return CacheLookup(hit=False)
        try:
            record = json.loads(raw)
        except json.JSONDecodeError:
            return CacheLookup(hit=False)
        if not isinstance(record, dict) or "value" not in record:
            return CacheLookup(hit=False)
        created_at = record.get("created_at")
        if ttl_seconds is not None and isinstance(created_at, str):
            from datetime import datetime

            try:
                created_ts = datetime.fromisoformat(created_at).timestamp()
            except ValueError:
                created_ts = None
            if created_ts is not None and (time.time() - created_ts) > ttl_seconds:
                return CacheLookup(hit=False)
        return CacheLookup(hit=True, value=record.get("value"), created_at=created_at)

    def put(self, key: str, value: Any, *, created_at: str) -> None:
        self._root.mkdir(parents=True, exist_ok=True)
        try:
            os.chmod(self._root, 0o700)
        except OSError:
            pass
        path = self._path(key)
        fd, tmp_name = tempfile.mkstemp(dir=self._root, prefix=".tmp-", suffix=".json")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump({"created_at": created_at, "value": value}, f)
            os.chmod(tmp_name, 0o600)
            os.replace(tmp_name, path)
        except Exception:
            try:
                os.unlink(tmp_name)
            except OSError:
                pass
            raise

    def clear(self) -> int:
        """Delete every cached entry under this store's directory. Returns
        the count removed; `0` when the directory doesn't exist yet."""
        if not self._root.exists():
            return 0
        count = 0
        for entry in self._root.glob("*.json"):
            try:
                entry.unlink()
            except OSError:
                continue
            count += 1
        return count

    def stats(self) -> dict[str, Any]:
        """`{entries, bytes, path}` — cheap: one `glob` + `stat` per entry,
        no cache content read."""
        if not self._root.exists():
            return {"entries": 0, "bytes": 0, "path": str(self._root)}
        entries = 0
        total_bytes = 0
        for entry in self._root.glob("*.json"):
            try:
                total_bytes += entry.stat().st_size
            except OSError:
                continue
            entries += 1
        return {"entries": entries, "bytes": total_bytes, "path": str(self._root)}
