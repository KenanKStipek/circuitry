from __future__ import annotations

import shlex
import subprocess
from dataclasses import dataclass
from typing import Any

from .base import ToolResult

_SHELL_METACHARACTERS = ("&&", "||", "|", ";", ">", "<", "`", "$(", "\n")


def _check_safe(value: str, field: str) -> None:
    for meta in _SHELL_METACHARACTERS:
        if meta in value:
            raise ValueError(
                f"ffmpeg params['{field}'] contains unsafe shell characters: {meta!r}. "
                "Only plain ffmpeg arguments are allowed."
            )


@dataclass(frozen=True)
class FfmpegPlugin:
    name: str = "ffmpeg"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        input_path = params.get("input")
        if not input_path or not isinstance(input_path, str):
            raise ValueError("FfmpegPlugin requires params['input'] as a non-empty string.")

        output_path = params.get("output")
        if not output_path or not isinstance(output_path, str):
            raise ValueError("FfmpegPlugin requires params['output'] as a non-empty string.")

        flags = str(params.get("flags", "")).strip()

        # Safety checks
        _check_safe(input_path, "input")
        _check_safe(output_path, "output")
        _check_safe(flags, "flags")

        # Build command: ffmpeg -y -i <input> [flags...] <output>
        cmd = ["ffmpeg", "-y", "-i", input_path]
        if flags:
            cmd += shlex.split(flags)
        cmd.append(output_path)

        try:
            proc = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=False,
                timeout=timeout_seconds,
            )
        except FileNotFoundError as e:
            raise RuntimeError(
                "ffmpeg is not installed or not on PATH. "
                "Install ffmpeg to use the ffmpeg plugin."
            ) from e
        except subprocess.TimeoutExpired as e:
            raise RuntimeError(
                f"ffmpeg timed out after {timeout_seconds}s"
            ) from e

        if proc.returncode != 0:
            raise RuntimeError(
                f"ffmpeg failed (exit {proc.returncode}): {proc.stderr.strip()}"
            )

        return ToolResult(
            value=output_path,
            raw={},
            stdout=proc.stdout,
            stderr=proc.stderr,
            exit_code=proc.returncode,
        )
