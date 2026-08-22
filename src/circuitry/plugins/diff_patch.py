"""Diff / patch tool plugin.

Two modes:
  - ``diff``: produce a unified diff between two strings or two files
    using stdlib ``difflib`` (no external binary required).
  - ``patch``: apply a unified-diff patch to a target file via the
    ``patch`` binary.

Params:
  - ``mode``: ``"diff"`` (default) or ``"patch"``.
  - ``from``: source string or path. Required for diff mode.
  - ``to``: target string or path. Required for diff mode.
  - ``from_path`` / ``to_path`` (bool, default False): when true, treat
    the corresponding param as a file path to read.
  - ``context`` (diff, int, default 3): unified-diff context lines.
  - ``patch`` (patch mode, str): the diff payload to apply.
  - ``target`` (patch mode, str): file path to patch.
"""

from __future__ import annotations

import difflib
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from ._subprocess import resolve_binary
from .base import ToolResult

_PATCH_CANDIDATES = ("patch",)


def _read_or_str(value: Any, *, is_path: bool, field: str) -> str:
    if not isinstance(value, str):
        raise ValueError(f"diff_patch: params['{field}'] must be a string.")
    if is_path:
        return Path(value).expanduser().read_text(encoding="utf-8")
    return value


@dataclass(frozen=True)
class DiffPatchPlugin:
    name: str = "diff_patch"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        mode = str(params.get("mode") or "diff").lower()
        if mode == "diff":
            return self._diff(params)
        if mode == "patch":
            return self._patch(params, timeout_seconds=timeout_seconds)
        raise ValueError(f"diff_patch: unknown mode {mode!r}")

    @staticmethod
    def _diff(params: dict[str, Any]) -> ToolResult:
        a = _read_or_str(
            params.get("from"), is_path=bool(params.get("from_path")), field="from"
        )
        b = _read_or_str(
            params.get("to"), is_path=bool(params.get("to_path")), field="to"
        )
        ctx = int(params.get("context", 3))
        diff_text = "".join(
            difflib.unified_diff(
                a.splitlines(keepends=True),
                b.splitlines(keepends=True),
                fromfile=str(params.get("from_label") or "a"),
                tofile=str(params.get("to_label") or "b"),
                n=ctx,
            )
        )
        return ToolResult(
            value=diff_text,
            raw={"mode": "diff", "context": ctx},
            stdout=None, stderr=None, exit_code=None,
        )

    @staticmethod
    def _patch(params: dict[str, Any], *, timeout_seconds: int) -> ToolResult:
        patch_text = params.get("patch")
        target = params.get("target")
        if not isinstance(patch_text, str) or not patch_text:
            raise ValueError("diff_patch: params['patch'] required.")
        if not isinstance(target, str) or not target:
            raise ValueError("diff_patch: params['target'] required.")
        binary = resolve_binary(_PATCH_CANDIDATES)
        if binary is None:
            raise RuntimeError("diff_patch: 'patch' binary not on PATH.")

        with tempfile.NamedTemporaryFile(
            "w", suffix=".patch", delete=False, encoding="utf-8"
        ) as tf:
            tf.write(patch_text)
            patch_file = tf.name
        try:
            proc = subprocess.run(
                [binary, "-p0", "-i", patch_file, str(target)],
                capture_output=True,
                text=True,
                timeout=int(timeout_seconds),
                check=False,
            )
        finally:
            Path(patch_file).unlink(missing_ok=True)

        if proc.returncode != 0:
            raise RuntimeError(
                f"diff_patch: patch failed (exit {proc.returncode}): "
                f"{(proc.stderr or proc.stdout).strip()}"
            )
        return ToolResult(
            value=str(target),
            raw={"mode": "patch"},
            stdout=proc.stdout,
            stderr=proc.stderr,
            exit_code=proc.returncode,
        )

    def check(self) -> CheckResult:
        # diff mode is stdlib-only; patch mode needs the binary.
        # Report ok unconditionally — patch is only required when the
        # patch mode is actually used at execute time.
        return CheckResult(
            ok=True,
            missing=[],
            message="diff mode uses stdlib difflib; patch mode requires "
                    "the 'patch' binary on PATH.",
        )
