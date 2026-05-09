"""Filesystem tool plugin — read, write, append, list, stat, exists, delete.

All operations stay within whatever path the caller specifies; there is
no implicit sandbox. The orchestration is responsible for vetting paths
that flow in from LLM output. The plugin refuses paths containing null
bytes (a common smuggling vector when paths are interpolated from
templates).

Params (depending on ``mode``):
  - ``mode``: one of ``read | write | append | list | stat | exists | delete``.
  - ``path``: target file or directory.
  - ``content`` (write/append, str): payload.
  - ``encoding`` (optional, default ``"utf-8"``).
  - ``create_dirs`` (write/append, bool, default ``True``): mkdir parents.
  - ``recursive`` (delete, bool, default ``False``).
"""

from __future__ import annotations

import os
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _validate_path(raw: Any) -> Path:
    if not isinstance(raw, str) or not raw:
        raise ValueError("fs: params['path'] must be a non-empty string.")
    if "\x00" in raw:
        raise ValueError("fs: path contains null byte.")
    return Path(raw).expanduser()


def _stat_dict(p: Path) -> dict[str, Any]:
    s = p.stat()
    return {
        "size": s.st_size,
        "mtime": int(s.st_mtime),
        "mode": s.st_mode,
        "is_dir": p.is_dir(),
        "is_file": p.is_file(),
    }


@dataclass(frozen=True)
class FsPlugin:
    name: str = "fs"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        mode = str(params.get("mode", "read")).lower()
        path = _validate_path(params.get("path"))
        encoding = str(params.get("encoding") or "utf-8")

        value: Any
        raw: dict[str, Any] = {"mode": mode, "path": str(path)}

        if mode == "read":
            if not path.is_file():
                raise FileNotFoundError(f"fs: not a file: {path}")
            value = path.read_text(encoding=encoding)

        elif mode in ("write", "append"):
            content = params.get("content")
            if not isinstance(content, str):
                raise ValueError("fs: write/append requires params['content'] as str.")
            if params.get("create_dirs", True):
                path.parent.mkdir(parents=True, exist_ok=True)
            with open(path, "a" if mode == "append" else "w", encoding=encoding) as fh:
                fh.write(content)
            value = str(path)
            raw["bytes_written"] = len(content.encode(encoding))

        elif mode == "list":
            if not path.is_dir():
                raise NotADirectoryError(f"fs: not a directory: {path}")
            value = sorted(p.name for p in path.iterdir())
            raw["count"] = len(value)

        elif mode == "stat":
            if not path.exists():
                raise FileNotFoundError(f"fs: path does not exist: {path}")
            value = _stat_dict(path)

        elif mode == "exists":
            value = path.exists()

        elif mode == "delete":
            if not path.exists():
                # Idempotent — deleting absent files is not an error.
                value = False
            elif path.is_dir():
                if not params.get("recursive", False):
                    raise IsADirectoryError(
                        f"fs: refusing to delete directory without recursive=True: {path}"
                    )
                shutil.rmtree(path)
                value = True
            else:
                os.unlink(path)
                value = True

        else:
            raise ValueError(f"fs: unknown mode {mode!r}")

        return ToolResult(
            value=value, raw=raw, stdout=None, stderr=None, exit_code=None
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
