"""DNS lookup tool plugin via dnspython.

Optional dep: ``dnspython``. Install with ``pip install circuitry-cof[dns]``.

Params:
  - ``domain`` (required): the name to resolve.
  - ``type`` (optional, default ``"A"``): record type
    (A, AAAA, MX, TXT, CNAME, NS, SOA, SRV, PTR, CAA).
  - ``nameservers`` (optional, list[str]): explicit DNS servers to query
    (e.g. ``["1.1.1.1"]``). Default: system resolver.

Returns ``value`` = list of records as strings (preserving native
representation: A → IPv4, MX → "10 mail.example.com", etc.).
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class DnsPlugin:
    name: str = "dns"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        try:
            import dns.resolver  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "dns: dnspython not installed. "
                "Install with: pip install dnspython"
            ) from exc

        domain = params.get("domain")
        if not isinstance(domain, str) or not domain.strip():
            raise ValueError("dns requires params['domain'].")
        rdtype = str(params.get("type") or "A").upper()
        nameservers = params.get("nameservers")

        resolver = dns.resolver.Resolver()
        resolver.lifetime = float(timeout_seconds)
        if isinstance(nameservers, list) and nameservers:
            resolver.nameservers = [str(n) for n in nameservers]

        try:
            answer = resolver.resolve(domain.strip(), rdtype)
        except dns.resolver.NXDOMAIN:
            return ToolResult(
                value=[],
                raw={"domain": domain, "type": rdtype, "error": "NXDOMAIN"},
                stdout=None, stderr="NXDOMAIN", exit_code=1,
            )
        except dns.resolver.NoAnswer:
            return ToolResult(
                value=[],
                raw={"domain": domain, "type": rdtype, "error": "NoAnswer"},
                stdout=None, stderr="NoAnswer", exit_code=2,
            )

        records = [str(r) for r in answer]
        return ToolResult(
            value=records,
            raw={
                "domain": domain,
                "type": rdtype,
                "ttl": int(answer.rrset.ttl) if answer.rrset is not None else None,
            },
            stdout=None, stderr=None, exit_code=0,
        )

    def check(self) -> CheckResult:
        # ``find_spec`` raises ModuleNotFoundError when probing a
        # submodule whose parent doesn't exist, so check the top-level
        # ``dns`` package first.
        try:
            present = importlib.util.find_spec("dns") is not None
        except ModuleNotFoundError:
            present = False
        if not present:
            return CheckResult(
                ok=False,
                missing=["library:dnspython"],
                message="pip install dnspython",
            )
        return CheckResult(ok=True, missing=[])
