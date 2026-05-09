"""Sandboxed shell tool plugin.

Runs a single binary from a configurable allowlist with arguments. NOT
a shell interpreter: ``shell=False`` is hard-coded and shell-meta
characters in args are rejected. The plugin is the safe equivalent of
``subprocess.run(["ls", "-la"], check=False)`` — it does NOT support
pipes, redirection, command substitution, or globbing.

Params:
  - ``command`` (required, str): the binary name to invoke. Must appear
    in ``allowed_commands`` (or the per-plugin default allowlist).
  - ``args`` (optional, list[str]): arguments. Each is passed verbatim;
    null bytes and ``\\n`` are rejected. Use ``stdin`` for newline-bearing
    input.
  - ``allowed_commands`` (optional, list[str]): per-effect override of
    the default allowlist. Use sparingly; the orchestration author is
    responsible for any commands they add.
  - ``cwd`` (optional, str).
  - ``stdin`` (optional, str).
  - ``allow_nonzero`` (optional, bool).

Default allowlist is intentionally tiny and read-only:
``("ls", "cat", "head", "tail", "wc", "echo", "pwd", "date")``.
Anything mutating the filesystem must be added explicitly per-effect.

AC C.5: a non-allowlisted command must be rejected before any side
effect — the binary is never invoked when the command isn't allowed.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from ._subprocess import resolve_binary, run_binary
from .base import ToolResult


_DEFAULT_ALLOWED: tuple[str, ...] = (
    "ls", "cat", "head", "tail", "wc", "echo", "pwd", "date",
)

_DANGEROUS_ARG_CHARS: tuple[str, ...] = ("\x00", "\n")


@dataclass(frozen=True)
class ShellPlugin:
    name: str = "shell"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        command = params.get("command")
        if not isinstance(command, str) or not command.strip():
            raise ValueError("shell requires params['command'] as a string.")
        command = command.strip()

        # The command MUST be a bare name — no slashes, whitespace, or
        # shell metacharacters. PATH lookup happens AFTER the allowlist
        # gate, so we don't have to worry about ``./malicious``-style
        # escapes. Allow alphanumerics plus ``-``/``_`` (covers binaries
        # like ``yt-dlp``, ``mediainfo``).
        if not command.replace("-", "").replace("_", "").isalnum():
            raise ValueError(
                f"shell: command {command!r} must be alphanumeric "
                "(with optional -_); use args for everything else."
            )

        allowed = params.get("allowed_commands")
        if allowed is None:
            allowed_set = set(_DEFAULT_ALLOWED)
        else:
            if not isinstance(allowed, list) or not all(
                isinstance(a, str) for a in allowed
            ):
                raise ValueError(
                    "shell: params['allowed_commands'] must be a list of strings."
                )
            allowed_set = set(allowed)

        if command not in allowed_set:
            raise PermissionError(
                f"shell: command {command!r} not in allowlist "
                f"{sorted(allowed_set)}. Add it via params['allowed_commands'] "
                "or pick a different command."
            )

        args = params.get("args") or []
        if not isinstance(args, list):
            raise ValueError("shell: params['args'] must be a list.")
        for i, a in enumerate(args):
            s = str(a)
            for bad in _DANGEROUS_ARG_CHARS:
                if bad in s:
                    raise ValueError(
                        f"shell: args[{i}] contains forbidden char {bad!r}."
                    )

        binary = resolve_binary([command])
        if binary is None:
            raise RuntimeError(f"shell: command {command!r} not found on PATH.")

        return run_binary(
            binary=binary,
            args=[str(a) for a in args],
            cwd=params.get("cwd"),
            stdin=params.get("stdin"),
            timeout_seconds=timeout_seconds,
            allow_nonzero=bool(params.get("allow_nonzero")),
        )

    def check(self) -> CheckResult:
        # The shell plugin itself has no binary — its allowlist is
        # checked at execute time. Report ok; preflight surfaces nothing.
        return CheckResult(
            ok=True,
            missing=[],
            message=(
                "shell allowlist defaults to read-only commands; pass "
                "params['allowed_commands'] to broaden per-effect."
            ),
        )
