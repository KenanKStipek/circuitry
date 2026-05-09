"""Zip archive tool plugin via stdlib zipfile.

Params:
  - ``mode``: ``"create" | "extract" | "list"``.
  - ``archive``: zip file path.
  - ``sources`` (create, list[str]): paths to include.
  - ``arcnames`` (create, optional list[str]): archived names matching
    ``sources``. Defaults to the basenames of ``sources``.
  - ``destination`` (extract, str): target dir (default current dir).

Refuses absolute or ``..``-containing entries on extract (zip-slip).
"""

from __future__ import annotations

import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _safe_member_path(name: str, dest: Path) -> Path:
    if name.startswith("/") or ".." in Path(name).parts:
        raise ValueError(f"zip: refusing unsafe member path {name!r}")
    target = (dest / name).resolve()
    if not str(target).startswith(str(dest.resolve())):
        raise ValueError(f"zip: refusing escape for {name!r}")
    return target


@dataclass(frozen=True)
class ZipPlugin:
    name: str = "zip"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        mode = str(params.get("mode") or "create").lower()
        archive_param = params.get("archive")
        if not isinstance(archive_param, str) or not archive_param:
            raise ValueError("zip requires params['archive'].")
        archive = Path(archive_param).expanduser()

        if mode == "create":
            sources = params.get("sources") or []
            if not isinstance(sources, list) or not sources:
                raise ValueError("zip: create requires params['sources'] non-empty.")
            arcnames = params.get("arcnames")
            if arcnames is not None and (
                not isinstance(arcnames, list) or len(arcnames) != len(sources)
            ):
                raise ValueError(
                    "zip: arcnames must be same-length list as sources."
                )
            archive.parent.mkdir(parents=True, exist_ok=True)
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
                for i, src in enumerate(sources):
                    src_path = Path(src).expanduser()
                    arcname = arcnames[i] if arcnames else src_path.name
                    if src_path.is_dir():
                        for sub in src_path.rglob("*"):
                            if sub.is_file():
                                zf.write(
                                    sub,
                                    arcname=str(Path(arcname) / sub.relative_to(src_path)),
                                )
                    else:
                        zf.write(src_path, arcname=arcname)
            return ToolResult(
                value=str(archive),
                raw={"sources": len(sources)},
                stdout=None, stderr=None, exit_code=None,
            )

        if mode == "list":
            with zipfile.ZipFile(archive, "r") as zf:
                names = zf.namelist()
            return ToolResult(
                value=names, raw={"count": len(names)},
                stdout=None, stderr=None, exit_code=None,
            )

        if mode == "extract":
            dest_param = params.get("destination") or "."
            dest = Path(str(dest_param)).expanduser()
            dest.mkdir(parents=True, exist_ok=True)
            extracted: list[str] = []
            with zipfile.ZipFile(archive, "r") as zf:
                for name in zf.namelist():
                    _safe_member_path(name, dest)
                    extracted.append(name)
                zf.extractall(dest)
            return ToolResult(
                value=str(dest), raw={"extracted": extracted},
                stdout=None, stderr=None, exit_code=None,
            )

        raise ValueError(f"zip: unknown mode {mode!r}")

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
