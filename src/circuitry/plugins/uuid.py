"""UUID generation tool plugin via stdlib uuid.

Params:
  - ``version``: ``1 | 4 | 5`` (default 4 — random).
  - ``namespace`` (v5 only): one of ``"dns" | "url" | "oid" | "x500"``
    or a string UUID; identifies the namespace for v5 UUIDs.
  - ``name`` (v5 only): name to hash within the namespace.
  - ``count`` (optional int, default 1): generate N UUIDs in one call.
  - ``hex`` (optional bool, default False): when true, returns the UUID
    without dashes.
"""

from __future__ import annotations

import uuid as _uuid
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


_NAMED_NAMESPACES: dict[str, _uuid.UUID] = {
    "dns": _uuid.NAMESPACE_DNS,
    "url": _uuid.NAMESPACE_URL,
    "oid": _uuid.NAMESPACE_OID,
    "x500": _uuid.NAMESPACE_X500,
}


@dataclass(frozen=True)
class UuidPlugin:
    name: str = "uuid"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        version = int(params.get("version") or 4)
        if version not in (1, 4, 5):
            raise ValueError(f"uuid: unsupported version {version}; expected 1|4|5.")
        count = int(params.get("count") or 1)
        if count < 1:
            raise ValueError("uuid: count must be >= 1.")
        as_hex = bool(params.get("hex"))

        ids: list[str] = []
        for _ in range(count):
            if version == 1:
                u = _uuid.uuid1()
            elif version == 5:
                ns = params.get("namespace")
                name = params.get("name")
                if not isinstance(name, str) or not name:
                    raise ValueError("uuid: v5 requires params['name'].")
                if isinstance(ns, str) and ns.lower() in _NAMED_NAMESPACES:
                    namespace = _NAMED_NAMESPACES[ns.lower()]
                elif isinstance(ns, str) and ns:
                    try:
                        namespace = _uuid.UUID(ns)
                    except ValueError as exc:
                        raise ValueError(
                            f"uuid: invalid v5 namespace {ns!r}: {exc}"
                        ) from exc
                else:
                    raise ValueError("uuid: v5 requires params['namespace'].")
                u = _uuid.uuid5(namespace, name)
            else:
                u = _uuid.uuid4()
            ids.append(u.hex if as_hex else str(u))

        value: Any = ids if count > 1 else ids[0]
        return ToolResult(
            value=value,
            raw={"version": version, "count": count, "hex": as_hex},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
