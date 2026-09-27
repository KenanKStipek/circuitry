"""SurrealDB tool plugin via the official ``surrealdb`` SDK.

Optional dep: ``surrealdb``. Install with
``pip install circuitry-cof[surrealdb]``.

Auth: credentials are read from the environment ONLY — ``SURREAL_USER`` +
``SURREAL_PASS``, or a pre-issued JWT in ``SURREAL_TOKEN``. They are never
read from ``runtime.plugins.surrealdb``, so they cannot reach
``runtime.effective_settings`` or persisted run state.

Config (``runtime.plugins.surrealdb``):
  - ``url``: RPC endpoint (default ``ws://localhost:8000/rpc``).
  - ``namespace`` / ``database``: bound with ``USE`` after signin. Either
    may be overridden per-effect via params.

Params:
  - ``mode``: ``"query" | "select" | "create" | "upsert" | "delete"``
    (default ``"query"``).
  - ``query`` (query): SurrealQL string.
  - ``params`` (query, optional): dict of SurrealQL variables.
  - ``target`` (select): table name or record id (``record`` / ``table``
    accepted as aliases).
  - ``table`` (create) + ``data`` (dict).
  - ``record`` (upsert) + ``data`` (dict).
  - ``record`` (delete).
  - ``namespace`` / ``database`` (optional): override the configured pair.
"""

from __future__ import annotations

import importlib.util
import os
import socket
import sys
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlsplit

from ..preflight import CheckResult
from .base import ToolResult

DEFAULT_URL = "ws://localhost:8000/rpc"
MODES = ("query", "select", "create", "upsert", "delete")

_DEFAULT_PORTS = {"ws": 8000, "wss": 443, "http": 8000, "https": 443}


def _load_sdk() -> Any:
    try:
        from surrealdb import Surreal  # type: ignore[import-not-found]
    except ImportError as exc:
        # Rich renders `[...]` as console markup, so the extras form is
        # spelled out rather than written as circuitry-cof[surrealdb].
        raise RuntimeError(
            "surrealdb: surrealdb SDK not installed. "
            "Install with: pip install surrealdb"
        ) from exc
    return Surreal


def _sdk_available() -> bool:
    """Whether the SDK is importable, without importing it.

    ``find_spec`` raises for an already-imported module that carries no
    ``__spec__`` (test doubles, namespace shims), so an in-``sys.modules``
    entry short-circuits the lookup.
    """
    module = sys.modules.get("surrealdb")
    if module is not None:
        return True
    try:
        return importlib.util.find_spec("surrealdb") is not None
    except (ImportError, ValueError):
        return False


def _credentials() -> dict[str, str]:
    """Resolve credentials from the environment.

    Returns either ``{"token": ...}`` or ``{"username": ..., "password": ...}``.
    """
    token = (os.environ.get("SURREAL_TOKEN") or "").strip()
    if token:
        return {"token": token}
    user = (os.environ.get("SURREAL_USER") or "").strip()
    password = os.environ.get("SURREAL_PASS") or ""
    if user and password:
        return {"username": user, "password": password}
    raise RuntimeError(
        "surrealdb: no credentials. Set SURREAL_USER and SURREAL_PASS, or "
        "SURREAL_TOKEN. Credentials are read from the environment only — "
        "never from runtime.plugins.surrealdb."
    )


def _is_connection_error(exc: BaseException) -> bool:
    """Best-effort split between 'server unreachable' and 'server said no'.

    The SDK wraps transport failures in its own exception types, so type
    checks alone under-report; the message sniff catches the wrapped cases.
    """
    if isinstance(exc, ConnectionError | TimeoutError | socket.gaierror | OSError):
        return True
    text = f"{type(exc).__name__}: {exc}".lower()
    return any(
        needle in text
        for needle in (
            "refused",
            "unreachable",
            "timed out",
            "timeout",
            "connection",
            "name or service not known",
            "failed to establish",
            "handshake",
        )
    )


def _jsonable(value: Any) -> Any:
    """Coerce SDK objects (RecordID, datetime, Decimal, ...) to JSON-safe types.

    Tool values land in run state, which is serialized to JSON; anything the
    encoder can't handle would fail the run *after* the write already
    happened server-side.
    """
    if value is None or isinstance(value, bool | int | float | str):
        return value
    if isinstance(value, dict):
        return {str(k): _jsonable(v) for k, v in value.items()}
    if isinstance(value, list | tuple | set):
        return [_jsonable(item) for item in value]
    return str(value)


def _raise_for_statement_errors(mode: str, response: Any) -> None:
    """SurrealQL statements can fail per-statement without raising."""
    if not isinstance(response, list):
        return
    for entry in response:
        if isinstance(entry, dict) and str(entry.get("status", "")).upper() == "ERR":
            detail = entry.get("result") or entry.get("detail") or entry
            raise RuntimeError(f"surrealdb {mode}: SurrealQL error: {detail}")


def _require_str(params: dict[str, Any], keys: tuple[str, ...], *, mode: str) -> str:
    for key in keys:
        value = params.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    expected = " / ".join(f"params[{k!r}]" for k in keys)
    raise ValueError(f"surrealdb {mode} requires {expected} as a non-empty string.")


def _require_data(params: dict[str, Any], *, mode: str) -> dict[str, Any]:
    data = params.get("data")
    if not isinstance(data, dict):
        raise ValueError(f"surrealdb {mode} requires params['data'] as a dict.")
    return data


def _build_call(mode: str, params: dict[str, Any]) -> Callable[[Any], Any]:
    """Validate params up front and return the SDK call to run once connected.

    Argument validation happens before any socket is opened so a malformed
    effect fails fast and identically whether or not a server is reachable.
    """
    if mode == "query":
        statement = _require_str(params, ("query",), mode="query")
        variables = params.get("params")
        if variables is not None and not isinstance(variables, dict):
            raise ValueError(
                "surrealdb query: params['params'] must be a dict of SurrealQL variables."
            )
        vars_dict: dict[str, Any] = dict(variables or {})

        def _query(db: Any) -> Any:
            return db.query(statement, vars_dict) if vars_dict else db.query(statement)

        return _query

    if mode == "select":
        target = _require_str(params, ("target", "record", "table"), mode="select")
        return lambda db: db.select(target)

    if mode == "create":
        table = _require_str(params, ("table", "target"), mode="create")
        data = _require_data(params, mode="create")
        return lambda db: db.create(table, data)

    if mode == "upsert":
        record = _require_str(params, ("record", "target"), mode="upsert")
        data = _require_data(params, mode="upsert")
        return lambda db: db.upsert(record, data)

    record = _require_str(params, ("record", "target"), mode="delete")
    return lambda db: db.delete(record)


def _signin_scopes(namespace: str, database: str) -> list[dict[str, str]]:
    """Namespace/database pairs to try in a signin payload, most specific first.

    SurrealDB's signin has no cross-level fallback server-side: a payload
    carrying both fields only matches a DATABASE-scoped user, one with just
    ``namespace`` only matches a NAMESPACE-scoped user, and a bare payload
    only matches a ROOT user. This plugin's ``namespace``/``database`` are
    mandatory config (needed for ``USE`` regardless of auth level), so there
    is no way to tell from config alone which kind of user is configured —
    trying all three levels client-side covers ROOT, NAMESPACE-scoped, and
    DATABASE-scoped deployments with a single credential.
    """
    scopes: list[dict[str, str]] = []
    if namespace and database:
        scopes.append({"namespace": namespace, "database": database})
    if namespace:
        scopes.append({"namespace": namespace})
    scopes.append({})
    return scopes


def _authenticate(
    db: Any,
    creds: dict[str, str],
    *,
    url: str,
    namespace: str = "",
    database: str = "",
) -> None:
    try:
        if "token" in creds:
            db.authenticate(creds["token"])
        else:
            last_exc: Exception = RuntimeError("surrealdb: no signin scope attempted")
            for scope in _signin_scopes(namespace, database):
                try:
                    db.signin({**creds, **scope})
                    break
                except Exception as exc:
                    if _is_connection_error(exc):
                        raise
                    last_exc = exc
            else:
                raise last_exc
    except Exception as exc:
        if _is_connection_error(exc):
            raise RuntimeError(_unreachable(url, exc)) from exc
        raise RuntimeError(
            f"surrealdb: authentication rejected by {url} ({exc}). Check "
            "SURREAL_TOKEN, or SURREAL_USER / SURREAL_PASS, and that the user "
            "has access to the configured namespace and database."
        ) from exc


def _unreachable(url: str, exc: BaseException) -> str:
    return (
        f"surrealdb: cannot connect to {url} ({exc}). Is the server running "
        "and is runtime.plugins.surrealdb.url correct? Start one locally with: "
        "surreal start --user root --pass root"
    )


@dataclass(frozen=True)
class SurrealDBPlugin:
    name: str = "surrealdb"
    url: str = DEFAULT_URL
    namespace: str = ""
    database: str = ""

    @property
    def base_url(self) -> str:
        """Surfaced as the effect's target in verbose/TUI output."""
        return self.url

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds  # SDK manages its own socket timeouts.

        mode = str(params.get("mode") or "query").lower()
        if mode not in MODES:
            raise ValueError(
                f"surrealdb: unknown mode {mode!r}. Expected one of: {', '.join(MODES)}."
            )

        call = _build_call(mode, params)

        namespace = (
            params.get("namespace")
            if isinstance(params.get("namespace"), str)
            else self.namespace
        )
        database = (
            params.get("database")
            if isinstance(params.get("database"), str)
            else self.database
        )
        if not namespace or not database:
            raise ValueError(
                "surrealdb requires a namespace and a database. Set them under "
                "runtime.plugins.surrealdb (namespace / database) or per-effect "
                "via params."
            )

        surreal = _load_sdk()
        creds = _credentials()

        try:
            db = surreal(self.url)
        except Exception as exc:
            if _is_connection_error(exc):
                raise RuntimeError(_unreachable(self.url, exc)) from exc
            raise RuntimeError(f"surrealdb: could not open {self.url} ({exc}).") from exc

        try:
            _authenticate(db, creds, url=self.url, namespace=namespace, database=database)
            try:
                db.use(namespace, database)
            except Exception as exc:
                raise RuntimeError(
                    f"surrealdb: USE {namespace} {database} failed ({exc}). "
                    "Check the namespace / database names and the user's grants."
                ) from exc

            try:
                response = call(db)
            except Exception as exc:
                if _is_connection_error(exc):
                    raise RuntimeError(_unreachable(self.url, exc)) from exc
                raise RuntimeError(f"surrealdb {mode} failed: {exc}") from exc

            _raise_for_statement_errors(mode, response)
        finally:
            close = getattr(db, "close", None)
            if callable(close):
                try:
                    close()
                except Exception:
                    pass

        return ToolResult(
            value=_jsonable(response),
            raw={
                "mode": mode,
                "url": self.url,
                "namespace": namespace,
                "database": database,
            },
            stdout=None,
            stderr=None,
            exit_code=None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if not _sdk_available():
            missing.append("library:surrealdb")

        if not (os.environ.get("SURREAL_TOKEN") or "").strip():
            if not (os.environ.get("SURREAL_USER") or "").strip():
                missing.append("env:SURREAL_USER")
            if not (os.environ.get("SURREAL_PASS") or ""):
                missing.append("env:SURREAL_PASS")

        if not _host_reachable(self.url):
            missing.append(f"host:{self.url}")

        if missing:
            return CheckResult(
                ok=False,
                missing=missing,
                message=(
                    "pip install surrealdb; export SURREAL_USER / SURREAL_PASS "
                    "(or SURREAL_TOKEN); surreal start --user root --pass root"
                ),
            )
        return CheckResult(ok=True, missing=[])


def _host_reachable(url: str, *, timeout: float = 2.0) -> bool:
    try:
        parts = urlsplit(url)
        host = parts.hostname
        if not host:
            return False
        port = parts.port or _DEFAULT_PORTS.get(parts.scheme, 8000)
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except Exception:
        return False
