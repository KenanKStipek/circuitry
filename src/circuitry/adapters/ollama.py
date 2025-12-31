from __future__ import annotations

import json
import shlex
import subprocess
from dataclasses import dataclass
from typing import Any

from .base import GenerateResult


@dataclass(frozen=True)
class OllamaAdapter:
    name: str = "ollama"
    base_url: str = "http://localhost:11434"

    def _curl_json(
        self,
        *,
        url: str,
        method: str = "GET",
        payload: dict[str, Any] | None = None,
        timeout_seconds: int = 120,
    ) -> dict[str, Any]:
        cmd = [
            "curl",
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--max-time",
            str(int(timeout_seconds)),
        ]

        if method.upper() == "POST":
            cmd += [
                "-H",
                "Content-Type: application/json",
                "-d",
                json.dumps(payload or {}),
            ]

        cmd.append(url)

        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        except FileNotFoundError as e:
            raise RuntimeError("curl is not installed or not on PATH") from e

        if proc.returncode != 0:
            cmd_str = " ".join(shlex.quote(c) for c in cmd)
            err = (proc.stderr or proc.stdout or "").strip()
            raise RuntimeError(
                f"curl failed (exit {proc.returncode}). cmd={cmd_str}. error={err}"
            )

        try:
            return json.loads(proc.stdout)
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"curl returned non-JSON response: {proc.stdout[:200]}"
            ) from e

    def list_models(self, *, timeout_seconds: int = 10) -> dict[str, Any]:
        url = self.base_url.rstrip("/") + "/api/tags"
        return self._curl_json(url=url, method="GET", timeout_seconds=timeout_seconds)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        url = self.base_url.rstrip("/") + "/api/generate"
        payload = {"model": model, "prompt": prompt, "stream": False}
        raw = self._curl_json(
            url=url, method="POST", payload=payload, timeout_seconds=timeout_seconds
        )
        return GenerateResult(text=(raw.get("response") or "").strip(), raw=raw)
