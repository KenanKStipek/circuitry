"""Trust for project config files discovered in the working directory.

A ``circuitry.config.json`` / ``config.json`` found in cwd can come with a
cloned repository, yet it carries the same authority as the user's own
config: adapter endpoints, tool binaries and environments, MCP server
commands, persistence targets, runtime plugins. So, like ``direnv allow``,
:func:`~circuitry.cli.config.resolve_config` applies a discovered file only
once the user has trusted it with ``cof trust``.

Trust lives in ``trusted.json`` beside the global ``config.json``: one entry
per file, keyed by its resolved absolute path, holding the SHA-256 of the
contents the user reviewed. Any edit changes the digest, and the file is
skipped again until it is re-trusted. Files the user names (``--config``,
``CIRCUITRY_CONFIG``) and the global config are always trusted; they never
reach this module.
"""

from __future__ import annotations

import hashlib
import json
import os
import shlex
import tempfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Literal

TRUST_STORE_FILENAME = "trusted.json"
TRUST_STORE_VERSION = 1

#: Trust every discovered project config without a trust entry. For CI and
#: containers where the checked-out repository is the user's own.
TRUST_PROJECT_CONFIG_ENV = "CIRCUITRY_TRUST_PROJECT_CONFIG"

#: ``trusted`` — the store holds this file with this digest.
#: ``env`` — trusted by :data:`TRUST_PROJECT_CONFIG_ENV`.
#: ``untrusted`` — no store entry.
#: ``changed`` — a store entry whose digest no longer matches the file.
TrustState = Literal["trusted", "env", "untrusted", "changed"]

TRUST_STATE_LABELS: dict[str, str] = {
    "trusted": "trusted",
    "env": f"trusted via {TRUST_PROJECT_CONFIG_ENV}",
    "untrusted": "not trusted — skipped",
    "changed": "changed since trusted — skipped",
}


class TrustStoreError(ValueError):
    """The trust store exists but cannot be read or rewritten.

    Message text is user-facing, like :class:`~circuitry.cli.config.ConfigError`.
    """


@dataclass(frozen=True)
class TrustEntry:
    """One trusted file as recorded in the store."""

    path: str
    sha256: str
    trusted_at: str


@dataclass(frozen=True)
class ProjectConfigStatus:
    """The project config :func:`resolve_config` discovered, and whether it applied."""

    path: Path
    trust: TrustState

    @property
    def applied(self) -> bool:
        return self.trust in ("trusted", "env")

    def describe(self) -> str:
        return f"{self.path} ({TRUST_STATE_LABELS[self.trust]})"

    def report(self) -> str:
        """:meth:`describe`, plus what to do about a skipped file (doctor views)."""
        if self.applied:
            return self.describe()
        return f"{self.describe()} — review it, then run `cof trust`"

    def skip_warning(self) -> str | None:
        """The one warning a skipped file produces, naming the command to run."""
        if self.applied:
            return None
        reason = (
            "it changed since you trusted it"
            if self.trust == "changed"
            else "it is not trusted"
        )
        return (
            f"Skipped project config {self.path}: {reason}, so none of its "
            f"settings apply. Review it, then run `cof trust "
            f"{shlex.quote(str(self.path))}` to apply it."
        )


def trust_key(path: Path) -> str:
    """The store key for *path*: its resolved absolute path."""
    return str(path.expanduser().resolve())


def config_digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def env_trusts_project_config() -> bool:
    raw = os.getenv(TRUST_PROJECT_CONFIG_ENV)
    return raw is not None and raw.strip().lower() in ("1", "true", "yes", "on")


def _read_store(store_path: Path) -> dict[str, TrustEntry]:
    """Every entry in the store; raises :class:`TrustStoreError` if unreadable."""
    try:
        text = store_path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return {}
    except (OSError, UnicodeDecodeError) as exc:
        raise TrustStoreError(f"Trust store {store_path} could not be read ({exc}).") from exc
    try:
        raw = json.loads(text)
    except json.JSONDecodeError as exc:
        raise TrustStoreError(
            f"Trust store {store_path} is not valid JSON ({exc.msg}, line {exc.lineno}); "
            "fix or delete it."
        ) from exc
    trusted = raw.get("trusted") if isinstance(raw, dict) else None
    if not isinstance(trusted, dict):
        raise TrustStoreError(
            f"Trust store {store_path} has no 'trusted' object; fix or delete it."
        )
    entries: dict[str, TrustEntry] = {}
    for key, value in trusted.items():
        if isinstance(value, dict) and isinstance(value.get("sha256"), str):
            entries[str(key)] = TrustEntry(
                path=str(key),
                sha256=value["sha256"],
                trusted_at=str(value.get("trusted_at") or ""),
            )
    return entries


def read_trust_entries(store_path: Path) -> list[TrustEntry]:
    """Every trusted file, sorted by path."""
    return sorted(_read_store(store_path).values(), key=lambda entry: entry.path)


def check_trust(path: Path, data: bytes, *, store_path: Path) -> TrustState:
    """Whether *path*, whose contents are *data*, may be applied.

    An unreadable store trusts nothing: the file is skipped and the warning
    tells the user to run ``cof trust``, which reports the broken store.
    """
    if env_trusts_project_config():
        return "env"
    try:
        entry = _read_store(store_path).get(trust_key(path))
    except TrustStoreError:
        return "untrusted"
    if entry is None:
        return "untrusted"
    return "trusted" if entry.sha256 == config_digest(data) else "changed"


def _write_store(store_path: Path, entries: dict[str, TrustEntry]) -> None:
    """Atomically replace the store; the file is 0600 in a directory created 0700."""
    store_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    payload: dict[str, Any] = {
        "version": TRUST_STORE_VERSION,
        "trusted": {
            key: {"sha256": entry.sha256, "trusted_at": entry.trusted_at}
            for key, entry in sorted(entries.items())
        },
    }
    fd, tmp_name = tempfile.mkstemp(
        dir=store_path.parent, prefix=".trusted.", suffix=".tmp"
    )
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(json.dumps(payload, indent=2) + "\n")
        os.replace(tmp_name, store_path)
    except BaseException:
        Path(tmp_name).unlink(missing_ok=True)
        raise


def record_trust(path: Path, data: bytes, *, store_path: Path) -> TrustEntry:
    """Trust *path* as long as its contents stay *data*."""
    entries = _read_store(store_path)
    key = trust_key(path)
    entry = TrustEntry(
        path=key,
        sha256=config_digest(data),
        trusted_at=datetime.now(timezone.utc).replace(microsecond=0).isoformat(),
    )
    entries[key] = entry
    _write_store(store_path, entries)
    return entry


def remove_trust(path: Path, *, store_path: Path) -> bool:
    """Forget *path*. Returns False when it was not trusted."""
    entries = _read_store(store_path)
    if entries.pop(trust_key(path), None) is None:
        return False
    _write_store(store_path, entries)
    return True


def flatten_settings(config: dict[str, Any], prefix: str = "") -> list[tuple[str, Any]]:
    """A config object as ``(dotted.key, leaf value)`` pairs, in file order."""
    pairs: list[tuple[str, Any]] = []
    for key, value in config.items():
        dotted = f"{prefix}.{key}" if prefix else str(key)
        if isinstance(value, dict) and value:
            pairs.extend(flatten_settings(value, dotted))
        else:
            pairs.append((dotted, value))
    return pairs


def host_sensitive_reason(dotted_key: str) -> str | None:
    """Why a setting reaches beyond the project, or None for an ordinary one."""
    parts = dotted_key.split(".")
    if parts[0] == "plugins":
        return "imports Python modules as runtime plugins"
    if parts[0] == "trust_orchestration_runtime":
        return "lets orchestration documents set host settings"
    if parts[0] != "runtime" or len(parts) < 2:
        return None
    section = parts[1]
    if section == "adapters":
        return "adapter setting: where prompts and credentials are sent"
    if section == "plugins":
        if len(parts) > 2 and parts[2] == "mcp":
            return "MCP server: a command Circuitry starts"
        if "binary" in parts[2:] or "env" in parts[2:]:
            return "tool binary or environment: what Circuitry executes"
        return "tool plugin setting"
    if section == "persistence":
        return "persistence: where run state is written"
    if section == "library":
        return "library source: where orchestrations are fetched from"
    return None
