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

import os
import shutil
import subprocess
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult, _as_bool


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


def plugin_binary_override(cfg: dict[str, Any]) -> str | None:
    """Read the optional ``binary`` setting from a plugin's
    ``runtime.plugins.<name>`` config block. ``~`` is expanded at
    resolution time (see :func:`resolve_plugin_binary`), not here."""
    value = cfg.get("binary")
    return str(value) if value else None


def plugin_env_override(cfg: dict[str, Any], *, plugin_name: str) -> dict[str, str] | None:
    """Read the optional ``env`` setting from a plugin's
    ``runtime.plugins.<name>`` config block."""
    value = cfg.get("env")
    if not value:
        return None
    if not isinstance(value, dict):
        raise ValueError(
            f"runtime.plugins.{plugin_name}.env must be a mapping of environment "
            f"variable names to values, got {type(value).__name__}."
        )
    return {str(k): str(v) for k, v in value.items()}


def merged_env(overrides: dict[str, str] | None) -> dict[str, str] | None:
    """Merge configured ``env`` overrides over the inherited environment.
    None (inherit unchanged) when no overrides are configured."""
    if not overrides:
        return None
    return {**os.environ, **overrides}


def _expand_and_validate(path_str: str) -> tuple[str, str | None]:
    """Expand ``~`` and validate a configured binary path.

    Returns ``(resolved_path, error)`` — ``error`` is ``None`` when the path
    is absolute, exists, and is executable; otherwise a short reason.
    """
    path = str(Path(path_str).expanduser())
    if not Path(path).is_absolute():
        return path, "must be an absolute path"
    if not (Path(path).is_file() and os.access(path, os.X_OK)):
        return path, "does not exist or is not executable"
    return path, None


def resolve_plugin_binary(
    *, plugin_name: str, candidates: Sequence[str], configured: str | None
) -> str:
    """Resolve the executable to run for a plugin.

    A configured ``binary`` overrides the ``PATH`` search entirely and must
    exist and be executable, or this raises naming the setting and the
    path. Unset, falls back to the first of ``candidates`` found on PATH.
    """
    if configured:
        path, error = _expand_and_validate(configured)
        if error:
            raise RuntimeError(
                f"{plugin_name}: configured runtime.plugins.{plugin_name}.binary"
                f"={configured!r} {error} (resolved: {path})."
            )
        return path
    binary = resolve_binary(candidates)
    if binary is None:
        raise RuntimeError(
            f"{plugin_name}: none of {list(candidates)} found on PATH. "
            f"Install one of them, or set runtime.plugins.{plugin_name}.binary."
        )
    return binary


def check_binary(
    candidates: Sequence[str],
    *,
    label: str | None = None,
    configured: str | None = None,
    plugin_name: str | None = None,
) -> CheckResult:
    """Standard preflight: report ``binary:<first-candidate>`` missing
    when none of the candidates are available on PATH, or — when a
    ``binary`` override is configured — when that path doesn't exist or
    isn't executable. ``plugin_name``, when given, names the
    ``runtime.plugins.<name>.binary`` setting in the not-found message —
    omit it for plugins (e.g. ``gpg``) that don't support the override."""
    primary = label or (candidates[0] if candidates else "?")
    if configured:
        path, error = _expand_and_validate(configured)
        if error is None:
            return CheckResult(ok=True, missing=[])
        return CheckResult(
            ok=False,
            missing=[f"binary:{primary}"],
            message=(
                f"configured runtime.plugins.{primary}.binary={configured!r} "
                f"{error} (resolved: {path})."
            ),
        )
    if resolve_binary(candidates):
        return CheckResult(ok=True, missing=[])
    message: str | None = None
    if plugin_name:
        message = (
            f"none of {list(candidates)} found on PATH. Install one of them, "
            f"or set runtime.plugins.{plugin_name}.binary."
        )
    elif len(candidates) > 1:
        message = f"none of {list(candidates)} found on PATH. Install one of them."
    return CheckResult(ok=False, missing=[f"binary:{primary}"], message=message)


def run_binary(
    *,
    binary: str,
    args: Sequence[str],
    cwd: str | None = None,
    stdin: str | None = None,
    env: dict[str, str] | None = None,
    timeout_seconds: int = 300,
    allow_nonzero: bool = False,
    not_found_hint: str | None = None,
) -> ToolResult:
    """Execute *binary* with *args* and return the result as a ToolResult.

    On non-zero exit, raises ``RuntimeError`` unless ``allow_nonzero``
    is set (in which case the failure is captured on the result and the
    caller can branch on ``exit_code``). ``FileNotFoundError`` is
    re-raised as a clearer ``RuntimeError`` so the missing binary case
    is unambiguous. ``not_found_hint``, when given, replaces the generic
    "override the plugin's binary path" suggestion with wording that
    names the caller's actual configuration setting.
    """
    cmd = [binary, *_validate_args(args)]
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
        hint = not_found_hint or "override the plugin's binary path"
        raise RuntimeError(
            f"binary not found: {binary!r} (install it or {hint})."
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

    ``binary`` and ``env`` come from ``runtime.plugins.<name>`` config
    (see :func:`plugin_binary_override` / :func:`plugin_env_override`),
    not params: a machine-specific executable path or thread-limit
    variable belongs in config, not in orchestration YAML.
    """

    name: str
    binary_candidates: tuple[str, ...]
    binary: str | None = None
    env: dict[str, str] | None = None

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
        binary = resolve_plugin_binary(
            plugin_name=self.name,
            candidates=self.binary_candidates,
            configured=self.binary,
        )
        return run_binary(
            binary=binary,
            args=args,
            cwd=params.get("cwd"),
            stdin=params.get("stdin"),
            env=merged_env(self.env),
            timeout_seconds=timeout_seconds,
            allow_nonzero=_as_bool(params.get("allow_nonzero")),
            not_found_hint=f"set runtime.plugins.{self.name}.binary",
        )

    def check(self) -> CheckResult:
        return check_binary(
            self.binary_candidates,
            label=self.name,
            configured=self.binary,
            plugin_name=self.name,
        )
