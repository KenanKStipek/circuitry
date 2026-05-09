"""Tar archive tool plugin via stdlib tarfile.

Params:
  - ``mode``: ``"create" | "extract" | "list"``.
  - ``archive``: archive file path.
  - ``sources`` (create, list[str]): paths to include.
  - ``arcnames`` (create, optional list[str]): archived names matching
    ``sources``. Defaults to the basenames of ``sources``.
  - ``destination`` (extract, str): target dir (default current dir).
  - ``compression`` (create, optional): ``"gz"`` | ``"bz2"`` | ``"xz"`` |
    ``""`` (default ``""`` = uncompressed).

The plugin refuses to extract entries containing ``..`` segments or
absolute paths (zip-slip / tar-slip protection).
"""

from __future__ import annotations

import tarfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _safe_extract_member(member: tarfile.TarInfo, dest: Path) -> Path:
    name = member.name
    if name.startswith("/") or ".." in Path(name).parts:
        raise ValueError(f"tar: refusing unsafe member path {name!r}")
    target = (dest / name).resolve()
    if not str(target).startswith(str(dest.resolve())):
        raise ValueError(f"tar: refusing escape-via-symlink for {name!r}")
    return target


@dataclass(frozen=True)
class TarPlugin:
    name: str = "tar"

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
            raise ValueError("tar requires params['archive'].")
        archive = Path(archive_param).expanduser()

        if mode == "create":
            sources = params.get("sources") or []
            if not isinstance(sources, list) or not sources:
                raise ValueError("tar: create requires params['sources'] non-empty.")
            arcnames = params.get("arcnames")
            if arcnames is not None and (
                not isinstance(arcnames, list) or len(arcnames) != len(sources)
            ):
                raise ValueError(
                    "tar: arcnames must be same-length list as sources."
                )
            compression = str(params.get("compression") or "")
            if compression not in ("", "gz", "bz2", "xz"):
                raise ValueError(f"tar: unsupported compression {compression!r}")
            mode_str = f"w:{compression}" if compression else "w"
            archive.parent.mkdir(parents=True, exist_ok=True)
            # ``mode_str`` is a runtime-built string; tarfile.open is
            # heavily overloaded on string Literal modes so mypy can't
            # match without a hint.
            with tarfile.open(archive, mode_str) as tf:  # type: ignore[call-overload]
                for i, src in enumerate(sources):
                    arcname = (
                        arcnames[i] if arcnames else Path(src).name
                    )
                    tf.add(Path(src).expanduser(), arcname=arcname)
            return ToolResult(
                value=str(archive),
                raw={"members": len(sources), "compression": compression},
                stdout=None, stderr=None, exit_code=None,
            )

        if mode == "list":
            with tarfile.open(archive, "r:*") as tf:
                names = tf.getnames()
            return ToolResult(
                value=names, raw={"count": len(names)},
                stdout=None, stderr=None, exit_code=None,
            )

        if mode == "extract":
            dest_param = params.get("destination") or "."
            dest = Path(str(dest_param)).expanduser()
            dest.mkdir(parents=True, exist_ok=True)
            extracted: list[str] = []
            with tarfile.open(archive, "r:*") as tf:
                for member in tf.getmembers():
                    _safe_extract_member(member, dest)
                    extracted.append(member.name)
                # filter="data" is the safer extraction mode introduced
                # in Python 3.12 and required (default in Python 3.14).
                tf.extractall(dest, filter="data")  # noqa: S202
            return ToolResult(
                value=str(dest), raw={"extracted": extracted},
                stdout=None, stderr=None, exit_code=None,
            )

        raise ValueError(f"tar: unknown mode {mode!r}")

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
