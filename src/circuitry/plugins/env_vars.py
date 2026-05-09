"""Read environment variables (read-only).

Writing to ``os.environ`` from an LLM-driven orchestration would be a
foot-gun (subprocesses inherit it, secrets propagate inadvertently),
so this plugin is deliberately read-only. To set vars, the orchestration
author should provide them via ``runtime`` config or the host shell.

Params:
  - ``mode``: ``"get" | "list"``.
  - ``name`` (get, str): variable name.
  - ``default`` (get, optional): returned when the var is unset.
  - ``prefix`` (list, optional): when set, only returns vars whose
    names start with this prefix.
  - ``include_secrets`` (list, default False): when False, vars whose
    names match the standard secret patterns (``API_KEY``, ``TOKEN``,
    ``PASSWORD``, ``SECRET``, ``CREDENTIALS``) are redacted in the output.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


_SECRET_PATTERN = re.compile(
    r"(?:API[_-]?KEY|TOKEN|PASSWORD|PASSWD|SECRET|CREDENTIAL|PRIVATE[_-]?KEY)",
    re.IGNORECASE,
)


@dataclass(frozen=True)
class EnvVarsPlugin:
    name: str = "env_vars"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        mode = str(params.get("mode") or "get").lower()

        if mode == "get":
            var_name = params.get("name")
            if not isinstance(var_name, str) or not var_name:
                raise ValueError("env_vars: get requires params['name'].")
            value = os.environ.get(var_name, params.get("default"))
            return ToolResult(
                value=value, raw={"name": var_name, "present": var_name in os.environ},
                stdout=None, stderr=None, exit_code=None,
            )

        if mode == "list":
            prefix = params.get("prefix")
            include_secrets = bool(params.get("include_secrets", False))
            items: dict[str, str] = {}
            for k, v in os.environ.items():
                if isinstance(prefix, str) and prefix and not k.startswith(prefix):
                    continue
                if not include_secrets and _SECRET_PATTERN.search(k):
                    items[k] = "***"
                else:
                    items[k] = v
            return ToolResult(
                value=items,
                raw={"count": len(items), "include_secrets": include_secrets},
                stdout=None, stderr=None, exit_code=None,
            )

        raise ValueError(f"env_vars: unknown mode {mode!r}")

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
