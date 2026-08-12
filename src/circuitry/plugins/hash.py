"""Hash tool plugin via stdlib hashlib.

Params:
  - ``algorithm``: ``"md5" | "sha1" | "sha224" | "sha256" | "sha384" |
    "sha512" | "sha3_256" | "sha3_512" | "blake2b" | "blake2s"``.
    Default ``"sha256"``. ``"md5"`` and ``"sha1"`` are accepted but
    only suitable for non-security uses (file integrity, cache keys).
  - ``input`` (str): payload to hash. When ``from_path`` is True, this is
    a file path read in 64KB chunks instead.
  - ``encoding`` (str, default ``"utf-8"``): when input is a string.
  - ``from_path`` (bool, default False).
  - ``output_format``: ``"hex"`` (default) or ``"base64"``.
"""

from __future__ import annotations

import base64 as _base64
import hashlib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult

_SUPPORTED_ALGORITHMS = {
    "md5", "sha1", "sha224", "sha256", "sha384", "sha512",
    "sha3_256", "sha3_512", "blake2b", "blake2s",
}


@dataclass(frozen=True)
class HashPlugin:
    name: str = "hash"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        algorithm = str(params.get("algorithm") or "sha256").lower()
        if algorithm not in _SUPPORTED_ALGORITHMS:
            raise ValueError(
                f"hash: unsupported algorithm {algorithm!r}. "
                f"Supported: {sorted(_SUPPORTED_ALGORITHMS)}"
            )
        output_format = str(params.get("output_format") or "hex").lower()
        if output_format not in ("hex", "base64"):
            raise ValueError("hash: output_format must be 'hex' or 'base64'.")

        try:
            hasher = hashlib.new(algorithm)
        except ValueError as exc:
            raise ValueError(f"hash: hashlib refused {algorithm!r}: {exc}") from exc

        if params.get("from_path"):
            src = params.get("input")
            if not isinstance(src, str) or not src:
                raise ValueError("hash: from_path=True requires params['input'] path.")
            path = Path(src).expanduser()
            with open(path, "rb") as fh:
                while True:
                    chunk = fh.read(65536)
                    if not chunk:
                        break
                    hasher.update(chunk)
        else:
            text = params.get("input")
            if not isinstance(text, str):
                raise ValueError("hash: in-memory mode requires params['input'] as str.")
            encoding = str(params.get("encoding") or "utf-8")
            hasher.update(text.encode(encoding))

        digest_bytes = hasher.digest()
        if output_format == "hex":
            value = digest_bytes.hex()
        else:
            value = _base64.b64encode(digest_bytes).decode("ascii")

        return ToolResult(
            value=value,
            raw={"algorithm": algorithm, "output_format": output_format},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
