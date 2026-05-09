"""System information tool plugin via psutil.

Optional dep: ``psutil``. Install with ``pip install circuitry-cof[psutil]``.

Params:
  - ``sections`` (optional, list[str]): subset of
    ``["cpu", "memory", "disk", "network", "boot", "load"]``.
    Default: all sections.

Returns ``value`` = dict keyed by section.
"""

from __future__ import annotations

import importlib.util
import os
import platform
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


_DEFAULT_SECTIONS = ("cpu", "memory", "disk", "network", "boot", "load")


@dataclass(frozen=True)
class SystemInfoPlugin:
    name: str = "system_info"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        try:
            import psutil  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "system_info: psutil not installed. "
                "Install with: pip install psutil"
            ) from exc

        sections = params.get("sections")
        if sections is None:
            section_set = set(_DEFAULT_SECTIONS)
        elif isinstance(sections, list) and all(isinstance(s, str) for s in sections):
            section_set = set(sections)
        else:
            raise ValueError(
                "system_info: params['sections'] must be a list of strings."
            )

        info: dict[str, Any] = {
            "platform": {
                "system": platform.system(),
                "release": platform.release(),
                "machine": platform.machine(),
                "python": platform.python_version(),
            }
        }
        if "cpu" in section_set:
            info["cpu"] = {
                "logical_cores": psutil.cpu_count(logical=True),
                "physical_cores": psutil.cpu_count(logical=False),
                "percent": psutil.cpu_percent(interval=0.1),
            }
        if "memory" in section_set:
            vm = psutil.virtual_memory()
            info["memory"] = {
                "total": int(vm.total),
                "available": int(vm.available),
                "percent": float(vm.percent),
            }
        if "disk" in section_set:
            du = psutil.disk_usage("/")
            info["disk"] = {
                "total": int(du.total),
                "used": int(du.used),
                "free": int(du.free),
                "percent": float(du.percent),
            }
        if "network" in section_set:
            net = psutil.net_io_counters()
            info["network"] = {
                "bytes_sent": int(net.bytes_sent),
                "bytes_recv": int(net.bytes_recv),
                "packets_sent": int(net.packets_sent),
                "packets_recv": int(net.packets_recv),
            }
        if "boot" in section_set:
            info["boot"] = {"boot_time": int(psutil.boot_time())}
        if "load" in section_set and hasattr(os, "getloadavg"):
            la1, la5, la15 = os.getloadavg()
            info["load"] = {"1m": la1, "5m": la5, "15m": la15}

        return ToolResult(
            value=info,
            raw={"sections": sorted(section_set)},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("psutil") is None:
            return CheckResult(
                ok=False,
                missing=["library:psutil"],
                message="pip install psutil",
            )
        return CheckResult(ok=True, missing=[])
