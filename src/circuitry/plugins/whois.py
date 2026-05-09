"""Whois lookup tool plugin via python-whois.

Optional dep: ``python-whois``. Install with
``pip install circuitry-cof[whois]``.

Params:
  - ``domain`` (required): the domain to look up.

Returns ``value`` = a dict of whois fields (registrar, creation_date,
expiration_date, name_servers, etc). Date fields are normalised to
ISO 8601 strings since python-whois returns datetime objects which
don't survive JSON serialisation.
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from datetime import date, datetime
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _to_jsonable(value: Any) -> Any:
    """Coerce datetime/date/non-str-iterable values into JSON-friendly form."""
    if isinstance(value, (datetime, date)):
        return value.isoformat()
    if isinstance(value, list):
        return [_to_jsonable(item) for item in value]
    if isinstance(value, dict):
        return {str(k): _to_jsonable(v) for k, v in value.items()}
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    # Fallback: stringify anything we don't recognise.
    return str(value)


@dataclass(frozen=True)
class WhoisPlugin:
    name: str = "whois"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        try:
            import whois  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "whois: python-whois not installed. "
                "Install with: pip install python-whois"
            ) from exc

        domain = params.get("domain")
        if not isinstance(domain, str) or not domain.strip():
            raise ValueError("whois requires params['domain'].")

        result = whois.whois(domain.strip())
        # python-whois returns a dict-like WhoisEntry; flatten it.
        as_dict: dict[str, Any] = (
            dict(result) if hasattr(result, "items") else {"raw": str(result)}
        )
        value = {k: _to_jsonable(v) for k, v in as_dict.items()}
        return ToolResult(
            value=value,
            raw={"domain": domain.strip()},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("whois") is None:
            return CheckResult(
                ok=False,
                missing=["library:python-whois"],
                message="pip install python-whois",
            )
        return CheckResult(ok=True, missing=[])
