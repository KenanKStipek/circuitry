from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest

from circuitry.plugins.base import ToolResult, validate_tool_result
from circuitry.plugins.ffmpeg import FfmpegPlugin

_FFMPEG_PATH = "/usr/bin/ffmpeg"


@pytest.fixture(autouse=True)
def _ffmpeg_on_path(monkeypatch: pytest.MonkeyPatch) -> None:
    """Most tests exercise command construction, not PATH resolution —
    stub ``ffmpeg`` as always resolvable so they don't depend on a real
    install. Tests for the missing-binary case override this."""
    monkeypatch.setattr(
        "circuitry.plugins._subprocess.shutil.which",
        lambda name: _FFMPEG_PATH if name == "ffmpeg" else None,
    )


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
    assert cmd[0] == _FFMPEG_PATH
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


def test_ffmpeg_raises_when_not_on_path(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("circuitry.plugins._subprocess.shutil.which", lambda name: None)
    with pytest.raises(RuntimeError, match="found on PATH"):
        FfmpegPlugin().execute(params={"input": "a.mp4", "output": "b.mp4"})


def test_ffmpeg_check_reports_missing_when_not_on_path(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr("circuitry.plugins._subprocess.shutil.which", lambda name: None)
    r = FfmpegPlugin().check()
    assert r.ok is False
    assert "binary:ffmpeg" in r.missing


def test_ffmpeg_check_ok_when_on_path() -> None:
    assert FfmpegPlugin().check().ok is True


# --- configured binary / env (runtime.plugins.ffmpeg) ---


def test_ffmpeg_configured_binary_runs_real_script(tmp_path: Path) -> None:
    """A configured ``binary`` bypasses PATH search entirely; prove it with
    a real temporary executable rather than depending on a real ffmpeg
    install being on the test machine."""
    script = tmp_path / "fake-ffmpeg"
    output = tmp_path / "out.mp4"
    script.write_text(
        "#!/bin/sh\n"
        f'echo "ARGS: $@" > "{output}"\n'
        f'echo "THREADS: $MAGICK_THREAD_LIMIT" >> "{output}"\n'
    )
    script.chmod(0o755)

    plugin = FfmpegPlugin(binary=str(script), env={"MAGICK_THREAD_LIMIT": "4"})
    result = plugin.execute(params={"input": "a.mp4", "output": str(output)})

    assert result.exit_code == 0
    assert result.raw["binary"] == str(script)
    written = output.read_text()
    assert "-y" in written and "-i" in written and "a.mp4" in written
    assert "THREADS: 4" in written


def test_ffmpeg_configured_binary_missing_fails_naming_setting_and_path(
    tmp_path: Path,
) -> None:
    missing = tmp_path / "nope"
    with pytest.raises(RuntimeError, match=r"runtime\.plugins\.ffmpeg\.binary.*nope"):
        FfmpegPlugin(binary=str(missing)).execute(
            params={"input": "a.mp4", "output": "b.mp4"}
        )


def test_ffmpeg_configured_binary_not_executable_fails_check(tmp_path: Path) -> None:
    not_exec = tmp_path / "not-exec"
    not_exec.write_text("#!/bin/sh\necho hi\n")
    not_exec.chmod(0o644)

    r = FfmpegPlugin(binary=str(not_exec)).check()
    assert r.ok is False
    assert "binary:ffmpeg" in r.missing
    assert "runtime.plugins.ffmpeg.binary" in (r.message or "")


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


# --- extra_inputs ---


def test_ffmpeg_extra_inputs_adds_multiple_i_flags(monkeypatch: pytest.MonkeyPatch) -> None:
    captured_cmd: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured_cmd.append(cmd)
        return FakeProc(returncode=0)

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", fake_run)

    FfmpegPlugin().execute(
        params={
            "input": "p1.png",
            "extra_inputs": ["p2.png", "p3.png"],
            "filter_complex": "hstack=inputs=3",
            "output": "strip.png",
        }
    )

    cmd = captured_cmd[0]
    assert cmd.count("-i") == 3
    assert "p1.png" in cmd
    assert "p2.png" in cmd
    assert "p3.png" in cmd
    assert "-filter_complex" in cmd
    assert "hstack=inputs=3" in cmd
    assert cmd[-1] == "strip.png"


# --- filter_complex ---


def test_ffmpeg_filter_complex_allows_semicolons(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", _fake_run_ok)
    # semicolons are valid in ffmpeg filter graph syntax — must not raise
    FfmpegPlugin().execute(
        params={
            "input": "a.png",
            "filter_complex": "[0:v]scale=512:512[out];[out]hflip[final]",
            "output": "b.png",
        }
    )


def test_ffmpeg_filter_complex_rejects_shell_injection() -> None:
    with pytest.raises(ValueError, match="unsafe"):
        FfmpegPlugin().execute(
            params={
                "input": "a.png",
                "filter_complex": "hstack=inputs=3 && rm -rf /",
                "output": "b.png",
            }
        )


# --- vf_drawtext ---


def test_ffmpeg_vf_drawtext_builds_drawtext_filter(monkeypatch: pytest.MonkeyPatch) -> None:
    captured_cmd: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured_cmd.append(cmd)
        return FakeProc(returncode=0)

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", fake_run)

    FfmpegPlugin().execute(
        params={
            "input": "panel.png",
            "vf_drawtext": {
                "text": "Hello world",
                "x": "(w-tw)/2",
                "y": "h-th-20",
                "fontsize": 24,
                "fontcolor": "white",
                "box": 1,
                "boxcolor": "black@0.75",
                "boxborderw": 8,
            },
            "output": "panel_text.png",
        }
    )

    cmd = captured_cmd[0]
    assert "-vf" in cmd
    vf_idx = cmd.index("-vf")
    vf_value = cmd[vf_idx + 1]
    assert vf_value.startswith("drawtext=")
    # text is wrapped in double quotes
    assert 'text="Hello world"' in vf_value
    assert "fontsize=24" in vf_value
    assert "fontcolor=white" in vf_value


def test_ffmpeg_vf_drawtext_escapes_colon_and_apostrophe(monkeypatch: pytest.MonkeyPatch) -> None:
    captured_cmd: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured_cmd.append(cmd)
        return FakeProc(returncode=0)

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", fake_run)

    FfmpegPlugin().execute(
        params={
            "input": "panel.png",
            "vf_drawtext": {"text": "It's time: now"},
            "output": "out.png",
        }
    )

    cmd = captured_cmd[0]
    vf_value = cmd[cmd.index("-vf") + 1]
    # Apostrophes are converted to Unicode right single quote to avoid ffmpeg avfilter quote parsing.
    # Colons inside double quotes are safe literals.
    assert 'text="It\u2019s time: now"' in vf_value


def test_ffmpeg_vf_drawtext_strips_surrounding_quotes(monkeypatch: pytest.MonkeyPatch) -> None:
    """LLMs sometimes include surrounding quotes in text values; they should be stripped."""
    captured_cmd: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured_cmd.append(cmd)
        return FakeProc(returncode=0)

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", fake_run)

    for text_with_quotes in ['"Should I be concerned?"', "'Nailed it.'"]:
        captured_cmd.clear()
        FfmpegPlugin().execute(
            params={"input": "p.png", "vf_drawtext": {"text": text_with_quotes}, "output": "o.png"}
        )
        vf_value = captured_cmd[0][captured_cmd[0].index("-vf") + 1]
        # outer quotes stripped — the rendered text should not start/end with quote chars
        assert '\\"' not in vf_value  # no escaped double-quote at start
        assert "text=\"Should I be" in vf_value or "text=\"Nailed it" in vf_value


def test_ffmpeg_vf_drawtext_strips_quote_before_trailing_period(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """'\"I have no regrets.\"' — period lands before the closing quote; both must be stripped."""
    captured_cmd: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured_cmd.append(cmd)
        return FakeProc(returncode=0)

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", fake_run)

    FfmpegPlugin().execute(
        params={"input": "p.png", "vf_drawtext": {"text": '"I have no regrets."'}, "output": "o.png"}
    )
    vf_value = captured_cmd[0][captured_cmd[0].index("-vf") + 1]
    assert "text=\"I have no regrets.\"" in vf_value
    # No extra escaped quote at the very start or end of the text value
    assert 'text="\\"' not in vf_value


def test_ffmpeg_vf_drawtext_collapses_newlines(monkeypatch: pytest.MonkeyPatch) -> None:
    """Multiline LLM text must be collapsed to a single line to avoid breaking the filter parser."""
    captured_cmd: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured_cmd.append(cmd)
        return FakeProc(returncode=0)

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", fake_run)

    FfmpegPlugin().execute(
        params={
            "input": "panel.png",
            "vf_drawtext": {
                "text": "Flesh and blood\nnow just a\nsnack.",
                "x": "(w-tw)/2",
                "y": "(h-th-50)",
                "fontsize": 22,
                "fontcolor": "white",
            },
            "output": "panel_text.png",
        }
    )

    cmd = captured_cmd[0]
    vf_value = cmd[cmd.index("-vf") + 1]
    # Newlines must be replaced with spaces
    assert "\n" not in vf_value
    assert 'text="Flesh and blood now just a snack."' in vf_value
    # Subsequent params must be intact (sanitized, no newlines)
    assert "y=(h-th-50)" in vf_value


def test_ffmpeg_vf_drawtext_apostrophe_with_newline(monkeypatch: pytest.MonkeyPatch) -> None:
    """Reproduces the exact error: apostrophe + newline in dialogue broke ffmpeg filter parsing."""
    captured_cmd: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured_cmd.append(cmd)
        return FakeProc(returncode=0)

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", fake_run)

    FfmpegPlugin().execute(
        params={
            "input": "panel.png",
            "vf_drawtext": {
                "text": "Wait, can't I...\ntalk?",
                "x": "(w-tw)/2",
                "y": 10,
                "fontsize": 24,
                "fontcolor": "black",
                "box": 1,
                "boxcolor": "gray@0.80",
                "boxborderw": 10,
            },
            "output": "panel_text.png",
        }
    )

    cmd = captured_cmd[0]
    vf_value = cmd[cmd.index("-vf") + 1]
    # No newlines anywhere in the filter string
    assert "\n" not in vf_value
    # Apostrophe replaced with Unicode right single quote
    assert "'" not in vf_value
    assert "\u2019" in vf_value
    # All params present and intact
    assert "y=10" in vf_value
    assert "fontsize=24" in vf_value


def test_ffmpeg_vf_drawtext_sanitizes_non_text_fields(monkeypatch: pytest.MonkeyPatch) -> None:
    """Newlines in non-text fields (e.g. y, fontsize) must be sanitized."""
    captured_cmd: list[list[str]] = []

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured_cmd.append(cmd)
        return FakeProc(returncode=0)

    monkeypatch.setattr("circuitry.plugins.ffmpeg.subprocess.run", fake_run)

    FfmpegPlugin().execute(
        params={
            "input": "panel.png",
            "vf_drawtext": {
                "text": "Hello",
                "y": "10\n",
                "fontsize": "24\r\n",
                "fontcolor": "white",
            },
            "output": "panel_text.png",
        }
    )

    cmd = captured_cmd[0]
    vf_value = cmd[cmd.index("-vf") + 1]
    assert "\n" not in vf_value
    assert "\r" not in vf_value
    assert "y=10" in vf_value
    assert "fontsize=24" in vf_value
