"""Shared subprocess helper for binary-wrapping tool plugins.

Most tool plugins in this catalog are thin shims around a CLI binary.
They share the same shape: validate args don't contain null bytes,
``subprocess.run`` with ``shell=False``, capture stdout/stderr/exit_code,
return as a :class:`ToolResult`. This module factors that pattern out so
each per-binary plugin file can be ~30 lines.

A :class:`GenericSubprocessTool` covers the common case where the
plugin's only job is to forward ``params['args']`` to the binary. Plugins
with richer semantics (``shell`` allowlist, ``gpg`` multi-mode, etc.)
construct their own commands and call :func:`run_binary` directly.
"""

from __future__ import annotations

import shutil
import subprocess
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _validate_args(args: Sequence[Any]) -> list[str]:
    """Coerce each arg to ``str`` and reject null bytes (subprocess refuses
    them anyway, but a clear error from us is more actionable)."""
    out: list[str] = []
    for i, a in enumerate(args):
        s = str(a)
        if "\x00" in s:
            raise ValueError(f"subprocess args[{i}] contains null byte.")
        out.append(s)
    return out


def resolve_binary(candidates: Sequence[str]) -> str | None:
    """Return the first candidate found on ``PATH`` (or absolute path
    that exists), else None. Useful for plugins like ``imagemagick``
    that ship under several names (``magick`` / ``convert``)."""
    for name in candidates:
        if not name:
            continue
        # absolute paths bypass PATH
        if Path(name).is_absolute() and Path(name).exists():
            return name
        found = shutil.which(name)
        if found:
            return found
    return None


def check_binary(candidates: Sequence[str], *, label: str | None = None) -> CheckResult:
    """Standard preflight: report ``binary:<first-candidate>`` missing
    when none of the candidates are available on PATH."""
    if resolve_binary(candidates):
        return CheckResult(ok=True, missing=[])
    primary = label or (candidates[0] if candidates else "?")
    return CheckResult(
        ok=False,
        missing=[f"binary:{primary}"],
        message=(
            f"none of {list(candidates)} found on PATH. Install the binary "
            "or set the per-plugin path override."
        ) if len(candidates) > 1 else None,
    )


def run_binary(
    *,
    binary: str,
    args: Sequence[str],
    cwd: str | None = None,
    stdin: str | None = None,
    env: dict[str, str] | None = None,
    timeout_seconds: int = 300,
    allow_nonzero: bool = False,
) -> ToolResult:
    """Execute *binary* with *args* and return the result as a ToolResult.

    On non-zero exit, raises ``RuntimeError`` unless ``allow_nonzero``
    is set (in which case the failure is captured on the result and the
    caller can branch on ``exit_code``). ``FileNotFoundError`` is
    re-raised as a clearer ``RuntimeError`` so the missing binary case
    is unambiguous.
    """
    cmd = [binary] + _validate_args(args)
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=int(timeout_seconds),
            cwd=cwd,
            env=env,
            input=stdin,
            check=False,
        )
    except FileNotFoundError as exc:
        raise RuntimeError(
            f"binary not found: {binary!r} (install it or override the "
            "plugin's binary path)."
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"{binary!r} exceeded timeout of {timeout_seconds}s"
        ) from exc

    if proc.returncode != 0 and not allow_nonzero:
        err = (proc.stderr or proc.stdout or "").strip()
        raise RuntimeError(
            f"{binary} failed (exit {proc.returncode}): {err}"
        )

    return ToolResult(
        value=proc.stdout,
        raw={"args": list(cmd[1:]), "cwd": cwd, "binary": binary},
        stdout=proc.stdout,
        stderr=proc.stderr,
        exit_code=proc.returncode,
    )


@dataclass(frozen=True)
class GenericSubprocessTool:
    """A pass-through ToolPlugin for binary-wrapping plugins whose only
    job is to forward ``params['args']`` to a named binary.

    Each plugin file instantiates this with its own (name, candidates).
    Configuration knobs are exposed via params at execute time:
      - ``args`` (required, list[str]).
      - ``cwd`` (optional, str).
      - ``stdin`` (optional, str): piped to the process.
      - ``allow_nonzero`` (optional, bool): when true, non-zero exit is
        captured on the result instead of raising.
    """

    name: str
    binary_candidates: tuple[str, ...]

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        args = params.get("args")
        if not isinstance(args, list):
            raise ValueError(
                f"{self.name}: params['args'] must be a list of strings."
            )
        binary = resolve_binary(self.binary_candidates)
        if binary is None:
            raise RuntimeError(
                f"{self.name}: none of {list(self.binary_candidates)} found on PATH."
            )
        return run_binary(
            binary=binary,
            args=args,
            cwd=params.get("cwd"),
            stdin=params.get("stdin"),
            timeout_seconds=timeout_seconds,
            allow_nonzero=bool(params.get("allow_nonzero")),
        )

    def check(self) -> CheckResult:
        return check_binary(self.binary_candidates, label=self.name)
