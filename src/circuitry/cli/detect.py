"""Backend detection — probes available LLM backends, tools, and models."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from dataclasses import dataclass, field
from typing import Any

from ..adapters.anthropic import AnthropicAdapter


@dataclass
class BackendStatus:
    """Result of probing a single backend."""

    name: str
    available: bool
    detail: str = ""
    models: list[str] = field(default_factory=list)


@dataclass
class DetectionResult:
    """Aggregate result of all backend probes."""

    backends: list[BackendStatus] = field(default_factory=list)

    @property
    def available_names(self) -> set[str]:
        return {b.name for b in self.backends if b.available}

    def get(self, name: str) -> BackendStatus | None:
        for b in self.backends:
            if b.name == name:
                return b
        return None


def _curl_json_get(url: str, timeout: int = 5) -> dict[str, Any]:
    """Quick GET via urllib (no extra deps). Raises on failure."""
    import urllib.request
    import urllib.error

    req = urllib.request.Request(url, method="GET")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read())


def detect_ollama(base_url: str = "http://localhost:11434") -> BackendStatus:
    """Probe Ollama at the given base URL."""
    try:
        data = _curl_json_get(f"{base_url.rstrip('/')}/api/tags")
        models_raw = data.get("models") or []
        names = [m.get("name", "") for m in models_raw if isinstance(m, dict)]
        return BackendStatus(
            name="ollama",
            available=True,
            detail=f"{base_url} ({len(names)} models)",
            models=names,
        )
    except Exception as e:
        return BackendStatus(name="ollama", available=False, detail=str(e))


def detect_openai() -> BackendStatus:
    """Check if OPENAI_API_KEY is set."""
    key = os.getenv("OPENAI_API_KEY", "")
    if key:
        return BackendStatus(
            name="openai",
            available=True,
            detail=f"API key set ({key[:8]}...)",
            models=["gpt-4o", "gpt-4o-mini", "gpt-4.1", "gpt-4.1-mini", "gpt-4.1-nano"],
        )
    return BackendStatus(name="openai", available=False, detail="OPENAI_API_KEY not set")


def detect_anthropic() -> BackendStatus:
    """Check if ANTHROPIC_API_KEY is set."""
    key = os.getenv("ANTHROPIC_API_KEY", "")
    if key:
        return BackendStatus(
            name="anthropic",
            available=True,
            detail=f"API key set ({key[:8]}...)",
            models=list(AnthropicAdapter.KNOWN_MODELS),
        )
    return BackendStatus(name="anthropic", available=False, detail="ANTHROPIC_API_KEY not set")


def detect_comfyui(base_url: str = "http://localhost:8188") -> BackendStatus:
    """Probe ComfyUI at the given base URL."""
    try:
        _curl_json_get(f"{base_url.rstrip('/')}/system_stats")
        return BackendStatus(
            name="comfyui",
            available=True,
            detail=base_url,
        )
    except Exception as e:
        return BackendStatus(name="comfyui", available=False, detail=str(e))


def detect_ffmpeg() -> BackendStatus:
    """Check if ffmpeg is on PATH."""
    path = shutil.which("ffmpeg")
    if path:
        try:
            result = subprocess.run(
                ["ffmpeg", "-version"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            first_line = result.stdout.split("\n")[0] if result.stdout else "found"
            return BackendStatus(name="ffmpeg", available=True, detail=first_line)
        except Exception:
            return BackendStatus(name="ffmpeg", available=True, detail=str(path))
    return BackendStatus(name="ffmpeg", available=False, detail="not found in PATH")


def detect_all(
    ollama_url: str = "http://localhost:11434",
    comfyui_url: str = "http://localhost:8188",
) -> DetectionResult:
    """Run all backend probes and return aggregated results."""
    return DetectionResult(
        backends=[
            detect_ollama(ollama_url),
            detect_openai(),
            detect_anthropic(),
            detect_comfyui(comfyui_url),
            detect_ffmpeg(),
        ]
    )
