from __future__ import annotations

import subprocess
from dataclasses import dataclass
from typing import Any

import pytest

from circuitry.plugins.base import ToolResult, validate_tool_result
from circuitry.plugins.ffmpeg import FfmpegPlugin


@dataclass
class FakeProc:
    returncode: int
    stdout: str = ""
    stderr: str = ""


def _fake_run_ok(*args: Any, **kwargs: Any) -> FakeProc:
    del args, kwargs
    return FakeProc(returncode=0, stdout="", stderr="")


def test_ffmpeg_executes_successfully(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", _fake_run_ok)

    plugin = FfmpegPlugin()
    result = plugin.execute(
        params={"input": "/in/video.mp4", "output": "/out/video.mp4"}
    )

    assert result.value == "/out/video.mp4"
    assert result.exit_code == 0
    assert isinstance(result.raw, dict)


def test_ffmpeg_injects_y_flag(monkeypatch: pytest.MonkeyPatch) -> None:
    captured_cmd: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured_cmd.append(cmd)
        return FakeProc(returncode=0)

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", fake_run)

    FfmpegPlugin().execute(params={"input": "a.mp4", "output": "b.mp4"})

    cmd = captured_cmd[0]
    assert cmd[0] == "ffmpeg"
    assert "-y" in cmd
    # -y must come before -i
    assert cmd.index("-y") < cmd.index("-i")


def test_ffmpeg_includes_flags(monkeypatch: pytest.MonkeyPatch) -> None:
    captured_cmd: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured_cmd.append(cmd)
        return FakeProc(returncode=0)

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", fake_run)

    FfmpegPlugin().execute(
        params={
            "input": "a.mp4",
            "output": "b.mp4",
            "flags": "-c:v libx264 -crf 23",
        }
    )

    cmd = captured_cmd[0]
    assert "-c:v" in cmd
    assert "libx264" in cmd
    assert "-crf" in cmd
    assert "23" in cmd
    assert cmd[-1] == "b.mp4"


def test_ffmpeg_raises_on_missing_input() -> None:
    with pytest.raises(ValueError, match="params\\['input'\\]"):
        FfmpegPlugin().execute(params={"output": "b.mp4"})


def test_ffmpeg_raises_on_missing_output() -> None:
    with pytest.raises(ValueError, match="params\\['output'\\]"):
        FfmpegPlugin().execute(params={"input": "a.mp4"})


def test_ffmpeg_rejects_shell_metacharacters_in_input() -> None:
    with pytest.raises(ValueError, match="unsafe shell"):
        FfmpegPlugin().execute(params={"input": "a.mp4 && rm -rf /", "output": "b.mp4"})


def test_ffmpeg_rejects_shell_metacharacters_in_flags() -> None:
    with pytest.raises(ValueError, match="unsafe shell"):
        FfmpegPlugin().execute(
            params={"input": "a.mp4", "output": "b.mp4", "flags": "-vf scale | evil"}
        )


def test_ffmpeg_raises_runtime_error_on_nonzero_exit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "circuitry.plugins.ffmpeg.subprocess.run",
        lambda *a, **kw: FakeProc(returncode=1, stderr="encoding failed"),
    )

    with pytest.raises(RuntimeError, match="ffmpeg failed"):
        FfmpegPlugin().execute(params={"input": "a.mp4", "output": "b.mp4"})


def test_ffmpeg_raises_when_not_installed(monkeypatch: pytest.MonkeyPatch) -> None:
    def raise_not_found(*args: Any, **kwargs: Any) -> Any:
        raise FileNotFoundError("ffmpeg not found")

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", raise_not_found)

    with pytest.raises(RuntimeError, match="not installed"):
        FfmpegPlugin().execute(params={"input": "a.mp4", "output": "b.mp4"})


def test_validate_tool_result_passes_for_valid_result() -> None:
    result = ToolResult(value="/out/video.mp4", raw={}, stdout="", stderr="", exit_code=0)
    assert validate_tool_result(result, plugin_name="ffmpeg") == []


def test_validate_tool_result_fails_for_bad_raw() -> None:
    result = ToolResult(value="x", raw="not-a-dict", exit_code=0)  # type: ignore[arg-type]
    diags = validate_tool_result(result, plugin_name="ffmpeg")
    assert any("raw" in d for d in diags)


def test_validate_tool_result_fails_for_negative_exit_code() -> None:
    result = ToolResult(value="x", raw={}, exit_code=-1)
    diags = validate_tool_result(result, plugin_name="ffmpeg")
    assert any("exit_code" in d for d in diags)
