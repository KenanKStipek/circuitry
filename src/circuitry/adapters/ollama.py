from __future__ import annotations

import json
import shlex
import shutil
import subprocess
import urllib.request
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
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
            # curl exit 7 = couldn't connect; 28 = timeout. Surface a hint that
            # names the next step instead of forcing the user to decode curl.
            hint = ""
            if proc.returncode in (7, 28):
                hint = (
                    f" Ollama at {self.base_url} is not reachable. "
                    "Start it (`ollama serve`), or set "
                    "`runtime.adapters.ollama.base_url` in your config. "
                    "Run `cof doctor` to verify connectivity."
                )
            raise RuntimeError(
                f"Ollama request failed (curl exit {proc.returncode}): {err}.{hint}"
                f" cmd={cmd_str}"
            )

        try:
            return json.loads(proc.stdout)
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"curl returned non-JSON response: {proc.stdout[:200]}"
            ) from e

    def list_models(self, *, timeout_seconds: float = 2.0) -> list[str]:
        """Locally installed tag names, e.g. ``["gpt-oss:20b", "phi3:mini"]``.

        Optional adapter hook (see
        :mod:`circuitry.adapters.models`): it exists to fill a picker, so
        an unreachable daemon is an empty list, not an error. stdlib
        urllib and a short timeout — nothing here is worth blocking a UI
        or requiring ``curl`` for.
        """
        url = self.base_url.rstrip("/") + "/api/tags"
        try:
            with urllib.request.urlopen(url, timeout=timeout_seconds) as resp:
                payload = json.loads(resp.read().decode("utf-8", errors="replace"))
        except Exception:
            return []

        entries = payload.get("models") if isinstance(payload, dict) else None
        if not isinstance(entries, list):
            return []
        names = [
            entry["name"].strip()
            for entry in entries
            if isinstance(entry, dict)
            and isinstance(entry.get("name"), str)
            and entry["name"].strip()
        ]
        return sorted(names)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        url = self.base_url.rstrip("/") + "/api/generate"
        payload = {"model": model, "prompt": prompt, "stream": False}
        raw = self._curl_json(
            url=url, method="POST", payload=payload, timeout_seconds=timeout_seconds
        )

        # Ollama commonly returns:
        # - prompt_eval_count (tokens processed for prompt)
        # - eval_count (tokens generated)
        tokens_sent = raw.get("prompt_eval_count")
        tokens_received = raw.get("eval_count")

        return GenerateResult(
            text=(raw.get("response") or "").strip(),
            raw=raw,
            tokens_sent=int(tokens_sent) if isinstance(tokens_sent, int) else None,
            tokens_received=int(tokens_received)
            if isinstance(tokens_received, int)
            else None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if shutil.which("curl") is None:
            missing.append("binary:curl")
        else:
            # Probe the Ollama daemon. Failure here is non-fatal at validate
            # time; doctor surfaces it as actionable.
            try:
                proc = subprocess.run(
                    [
                        "curl",
                        "--silent",
                        "--max-time",
                        "2",
                        "--head",
                        self.base_url.rstrip("/") + "/api/tags",
                    ],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                if proc.returncode != 0:
                    missing.append(f"host:{self.base_url}")
            except Exception:
                missing.append(f"host:{self.base_url}")
        return CheckResult(ok=not missing, missing=missing)
