from __future__ import annotations

import shlex
import subprocess
from dataclasses import dataclass
from typing import Any

from .base import ToolResult

_SHELL_METACHARACTERS = ("&&", "||", "|", ";", ">", "<", "`", "$(", "\n")

# filter_complex allows | and ; (ffmpeg filter graph separators) but still blocks shell injection
_FILTER_METACHARACTERS = ("&&", ">", "<", "`", "$(", "\n")


def _check_safe(value: str, field: str) -> None:
    for meta in _SHELL_METACHARACTERS:
        if meta in value:
            raise ValueError(
                f"ffmpeg params['{field}'] contains unsafe shell characters: {meta!r}. "
                "Only plain ffmpeg arguments are allowed."
            )


def _check_filter_safe(value: str, field: str) -> None:
    """Less restrictive check for filter_complex — allows | and ; as ffmpeg filter syntax."""
    for meta in _FILTER_METACHARACTERS:
        if meta in value:
            raise ValueError(
                f"ffmpeg params['{field}'] contains unsafe characters: {meta!r}. "
                "Only ffmpeg filter graph expressions are allowed."
            )


def _escape_drawtext_text(text: str) -> str:
    """Escape text for use in an ffmpeg drawtext filter (unquoted form)."""
    text = text.replace("\\", "\\\\")
    text = text.replace("\n", " ")
    text = text.replace("'", "\\'")
    text = text.replace(":", "\\:")
    return text


def _build_drawtext_filter(cfg: dict[str, Any]) -> str:
    """Build a drawtext= filter string from a config dict, with text properly escaped."""
    text_escaped = _escape_drawtext_text(str(cfg.get("text", "")))
    parts = [f"text={text_escaped}"]
    for key in ("x", "y", "fontsize", "fontfile", "fontcolor", "box", "boxcolor", "boxborderw"):
        if key in cfg:
            parts.append(f"{key}={cfg[key]}")
    return "drawtext=" + ":".join(parts)


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
        extra_inputs: list[Any] = params.get("extra_inputs") or []
        filter_complex: str | None = params.get("filter_complex")
        map_out: str | None = params.get("map")
        vf_drawtext: dict[str, Any] | None = params.get("vf_drawtext")

        # Safety checks
        _check_safe(input_path, "input")
        _check_safe(output_path, "output")
        if flags:
            _check_safe(flags, "flags")
        for i, extra in enumerate(extra_inputs):
            _check_safe(str(extra), f"extra_inputs[{i}]")
        if filter_complex is not None:
            if not isinstance(filter_complex, str):
                raise ValueError("FfmpegPlugin: params['filter_complex'] must be a string.")
            _check_filter_safe(filter_complex, "filter_complex")

        # Build command: ffmpeg -y -i <input> [-i <extra>...] [filter/flags...] [-map ...] <output>
        cmd = ["ffmpeg", "-y", "-i", input_path]
        for extra in extra_inputs:
            cmd += ["-i", str(extra)]

        if filter_complex is not None:
            cmd += ["-filter_complex", filter_complex]
        elif vf_drawtext and isinstance(vf_drawtext, dict):
            cmd += ["-vf", _build_drawtext_filter(vf_drawtext)]
        elif flags:
            cmd += shlex.split(flags)

        if map_out:
            cmd += ["-map", str(map_out)]

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
