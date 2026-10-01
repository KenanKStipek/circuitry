from __future__ import annotations

import json
import shutil
import subprocess
import urllib.request
from dataclasses import dataclass
from typing import Any, ClassVar

from ..preflight import CheckResult
from ._curl_errors import curl_failure_message, parse_error_body
from .base import (
    DETERMINISTIC_SEED,
    GenerateOptions,
    GenerateResult,
    ImageInput,
    last_user_index,
)

#: ``params`` keys that are request fields in Ollama's API; every other key
#: is a model option and goes under ``options``.
_REQUEST_FIELDS = frozenset({"format", "keep_alive", "think"})


def _image_base64(image: ImageInput) -> str:
    if image.data is None:
        raise RuntimeError(
            f"ollama cannot fetch an image URL ({image.url}); "
            "download it and give the asset a local path."
        )
    return image.base64_data()


def _request(*, model: str, prompt: str, options: GenerateOptions) -> tuple[str, dict[str, Any]]:
    """``(endpoint, body)``: ``/api/chat`` for role-tagged turns, else ``/api/generate``."""
    payload: dict[str, Any] = {"model": model, "stream": False}
    images = [_image_base64(image) for image in options.images]
    if options.messages:
        endpoint = "/api/chat"
        messages: list[dict[str, Any]] = [
            {"role": m.role, "content": m.content} for m in options.messages
        ]
        if images:
            index = last_user_index(messages)
            if index is None:
                messages.append({"role": "user", "content": ""})
                index = len(messages) - 1
            messages[index]["images"] = images
        payload["messages"] = messages
    else:
        endpoint = "/api/generate"
        payload["prompt"] = prompt
        if images:
            payload["images"] = images

    model_options: dict[str, Any] = {}
    if options.temperature is not None:
        model_options["temperature"] = options.temperature
    if options.max_tokens is not None:
        model_options["num_predict"] = options.max_tokens
    if options.stop:
        model_options["stop"] = list(options.stop)
    for key, value in options.params.items():
        if key in _REQUEST_FIELDS:
            payload[key] = value
        else:
            model_options[key] = value
    if options.deterministic:
        model_options.setdefault("seed", DETERMINISTIC_SEED)
    if model_options:
        payload["options"] = model_options
    return endpoint, payload


@dataclass(frozen=True)
class OllamaAdapter:
    accepts_images: ClassVar[bool] = True
    name: str = "ollama"
    base_url: str = "http://localhost:11434"

    def _curl_json(
        self,
        *,
        url: str,
        method: str = "GET",
        payload: dict[str, Any] | None = None,
        timeout_seconds: int = 120,
        model: str | None = None,
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
            # curl exit 7 = couldn't connect (daemon down / wrong base_url);
            # 28 = --max-time elapsed (daemon reached, still generating);
            # 22 (--fail-with-body) = HTTP 4xx/5xx, body already parsed into
            # the message below. These are different problems — surface a
            # hint that names the actual next step instead of forcing the
            # user to decode curl, and don't tell someone whose server
            # answered that it isn't reachable.
            hint = ""
            if proc.returncode == 7:
                hint = (
                    f"Ollama at {self.base_url} is not reachable. "
                    "Start it (`ollama serve`), or set "
                    "`runtime.adapters.ollama.base_url` in your config. "
                    "Run `cof doctor` to verify connectivity."
                )
            elif proc.returncode == 28:
                hint = (
                    f"The model didn't finish within {int(timeout_seconds)}s. "
                    "Raise the prompt's `timeout_ms` or "
                    "`runtime.adapters.ollama.timeout_seconds` in your "
                    "config, or use a smaller/faster model."
                )
            elif proc.returncode == 22:
                body_message = parse_error_body(proc.stdout) or ""
                if model and "not found" in body_message.lower():
                    hint = f"Try `ollama pull {model}` first."
            raise RuntimeError(
                curl_failure_message(
                    adapter="ollama",
                    model=model,
                    url=url,
                    returncode=proc.returncode,
                    stdout=proc.stdout,
                    stderr=proc.stderr,
                    hint=hint,
                )
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
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        endpoint, payload = _request(
            model=model, prompt=prompt, options=options or GenerateOptions()
        )
        url = self.base_url.rstrip("/") + endpoint
        raw = self._curl_json(
            url=url,
            method="POST",
            payload=payload,
            timeout_seconds=timeout_seconds,
            model=model,
        )

        # Ollama commonly returns:
        # - prompt_eval_count (tokens processed for prompt)
        # - eval_count (tokens generated)
        tokens_sent = raw.get("prompt_eval_count")
        tokens_received = raw.get("eval_count")
        done_reason = raw.get("done_reason")
        message = raw.get("message")
        text = (
            message.get("content") if isinstance(message, dict) else raw.get("response")
        )

        return GenerateResult(
            text=(text or "").strip(),
            raw=raw,
            tokens_sent=int(tokens_sent) if isinstance(tokens_sent, int) else None,
            tokens_received=int(tokens_received)
            if isinstance(tokens_received, int)
            else None,
            finish_reason=done_reason if isinstance(done_reason, str) else None,
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
