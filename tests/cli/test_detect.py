"""Tests for backend detection module."""

from __future__ import annotations

import json
from unittest.mock import patch

from circuitry.cli.detect import (
    BackendStatus,
    DetectionResult,
    detect_all,
    detect_anthropic,
    detect_ffmpeg,
    detect_ollama,
    detect_openai,
)


# --- DetectionResult ---


def test_detection_result_available_names() -> None:
    result = DetectionResult(
        backends=[
            BackendStatus(name="ollama", available=True),
            BackendStatus(name="openai", available=False),
            BackendStatus(name="ffmpeg", available=True),
        ]
    )
    assert result.available_names == {"ollama", "ffmpeg"}


def test_detection_result_get() -> None:
    result = DetectionResult(
        backends=[
            BackendStatus(name="ollama", available=True, detail="ok"),
        ]
    )
    assert result.get("ollama") is not None
    assert result.get("ollama").available is True
    assert result.get("nonexistent") is None


# --- Ollama ---


def test_detect_ollama_success() -> None:
    mock_data = {"models": [{"name": "llama3:latest"}, {"name": "phi3:mini"}]}

    with patch("circuitry.cli.detect._curl_json_get", return_value=mock_data):
        status = detect_ollama("http://localhost:11434")

    assert status.available is True
    assert status.name == "ollama"
    assert len(status.models) == 2
    assert "llama3:latest" in status.models


def test_detect_ollama_failure() -> None:
    with patch("circuitry.cli.detect._curl_json_get", side_effect=ConnectionError("refused")):
        status = detect_ollama()

    assert status.available is False
    assert "refused" in status.detail


# --- OpenAI ---


def test_detect_openai_with_key() -> None:
    with patch.dict("os.environ", {"OPENAI_API_KEY": "sk-test1234567890"}):
        status = detect_openai()

    assert status.available is True
    assert len(status.models) > 0


def test_detect_openai_without_key() -> None:
    with patch.dict("os.environ", {}, clear=True):
        status = detect_openai()

    assert status.available is False


# --- Anthropic ---


def test_detect_anthropic_with_key() -> None:
    with patch.dict("os.environ", {"ANTHROPIC_API_KEY": "sk-ant-test1234"}):
        status = detect_anthropic()

    assert status.available is True
    assert len(status.models) > 0


def test_detect_anthropic_without_key() -> None:
    with patch.dict("os.environ", {}, clear=True):
        status = detect_anthropic()

    assert status.available is False


# --- ffmpeg ---


def test_detect_ffmpeg_available() -> None:
    with patch("shutil.which", return_value="/usr/bin/ffmpeg"):
        with patch("subprocess.run") as mock_run:
            mock_run.return_value.stdout = "ffmpeg version 7.1"
            status = detect_ffmpeg()

    assert status.available is True


def test_detect_ffmpeg_not_found() -> None:
    with patch("shutil.which", return_value=None):
        status = detect_ffmpeg()

    assert status.available is False


# --- detect_all ---


def test_detect_all_returns_all_backends() -> None:
    mock_data = {"models": []}
    with patch("circuitry.cli.detect._curl_json_get", side_effect=[mock_data, ConnectionError()]):
        with patch("shutil.which", return_value=None):
            with patch.dict("os.environ", {}, clear=True):
                result = detect_all()

    assert len(result.backends) == 5
    names = {b.name for b in result.backends}
    assert names == {"ollama", "openai", "anthropic", "comfyui", "ffmpeg"}
