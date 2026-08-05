"""Adapter for CyberDiner — a job-queue LLM broker.

Unlike the OpenAI-compatible adapters, CyberDiner's expo service is
async: a prompt is submitted as a job and the result is retrieved by
polling. Wire shape mirrors cookd's ask client (``apps/cook/cookd/src/
commands/ask.rs`` in KenanKStipek/CyberDiner): ``POST {expo_url}/beta/
jobs`` with ``{prompt, tier}`` and a bearer token returns a ``job_id``;
``GET {expo_url}/beta/jobs/{job_id}`` is polled until the job leaves the
in-flight states (``pending``/``assigned``/``running``) and reaches a
terminal one (``complete``/``failed``/``cancelled``).

Authentication: a CyberDiner API key (``ck_...``, minted via expo's
``api_keys`` routes or the web app), supplied as
``runtime.adapters.cyberdiner.token``. The adapter treats it as an
opaque bearer string — no prefix validation; expo's auth middleware
decides what's valid.

Uses stdlib ``urllib.request`` only (see ``plugins/http.py`` for the
same idiom) — no new dependency for a single job-broker adapter.
"""

from __future__ import annotations

import json
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import GenerateResult

_VALID_TIERS = ("tier-1", "tier-2", "tier-3", "tier-4")
_TERMINAL_STATUSES = frozenset({"complete", "failed", "cancelled"})


def _resolve_tier(model: str, default_tier: str) -> str:
    tier = (model or "").strip() or default_tier
    if tier not in _VALID_TIERS:
        valid = ", ".join(_VALID_TIERS)
        raise ValueError(f"cyberdiner: unknown tier {tier!r}. Valid tiers: {valid}.")
    return tier


@dataclass(frozen=True)
class CyberdinerAdapter:
    name: str = "cyberdiner"
    expo_url: str = ""
    token: str = ""
    default_tier: str = "tier-1"
    poll_interval_ms: int = 500
    # Per-HTTP-request socket timeout; distinct from generate()'s
    # timeout_seconds, which bounds the whole submit+poll sequence.
    timeout_seconds: int = 30

    def _request(
        self,
        *,
        method: str,
        url: str,
        payload: dict[str, Any] | None,
        timeout: float,
    ) -> dict[str, Any]:
        body = json.dumps(payload).encode("utf-8") if payload is not None else None
        req = urllib.request.Request(url, data=body, method=method)
        req.add_header("Authorization", f"Bearer {self.token}")
        if body is not None:
            req.add_header("Content-Type", "application/json")

        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                text = resp.read().decode("utf-8", errors="replace")
        except urllib.error.HTTPError as exc:
            detail = ""
            try:
                detail = exc.read().decode("utf-8", errors="replace") if exc.fp else ""
            except Exception:
                detail = ""
            raise RuntimeError(
                f"cyberdiner: HTTP {exc.code} from {url}: {exc.reason}"
                + (f" — {detail}" if detail else "")
            ) from exc
        except urllib.error.URLError as exc:
            raise RuntimeError(
                f"cyberdiner: request to {url} failed: {exc.reason}"
            ) from exc

        try:
            return dict(json.loads(text)) if text else {}
        except json.JSONDecodeError as exc:
            raise RuntimeError(
                f"cyberdiner: non-JSON response from {url}: {text[:200]}"
            ) from exc

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if not self.expo_url:
            raise RuntimeError(
                "cyberdiner: expo_url not configured. Set "
                "runtime.adapters.cyberdiner.expo_url to your CyberDiner "
                "expo deployment's root URL."
            )
        if not self.token:
            raise RuntimeError(
                "cyberdiner: token not configured. Set "
                "runtime.adapters.cyberdiner.token to a CyberDiner API key "
                "(ck_...), minted via expo's api_keys routes or the web app."
            )

        tier = _resolve_tier(model, self.default_tier)
        base = self.expo_url.rstrip("/")
        req_timeout = max(0.001, min(float(self.timeout_seconds), float(timeout_seconds)))
        deadline = time.monotonic() + timeout_seconds

        job = self._request(
            method="POST",
            url=f"{base}/beta/jobs",
            payload={"prompt": prompt, "tier": tier},
            timeout=req_timeout,
        )
        job_id = job.get("job_id")
        if not job_id:
            raise RuntimeError(f"cyberdiner: submit response missing job_id: {job!r}")

        poll_interval_seconds = max(0.0, self.poll_interval_ms) / 1000.0
        status = job.get("status")
        while status not in _TERMINAL_STATUSES:
            if time.monotonic() >= deadline:
                raise RuntimeError(
                    f"cyberdiner: timed out after {timeout_seconds}s waiting for "
                    f"job {job_id} to complete (last status={status!r})."
                )
            time.sleep(poll_interval_seconds)
            if time.monotonic() >= deadline:
                raise RuntimeError(
                    f"cyberdiner: timed out after {timeout_seconds}s waiting for "
                    f"job {job_id} to complete (last status={status!r})."
                )
            job = self._request(
                method="GET",
                url=f"{base}/beta/jobs/{job_id}",
                payload=None,
                timeout=req_timeout,
            )
            status = job.get("status")

        if status == "failed":
            error_message = job.get("error_message") or "unknown error"
            raise RuntimeError(f"cyberdiner: job {job_id} failed: {error_message}")
        if status == "cancelled":
            raise RuntimeError(f"cyberdiner: job {job_id} was cancelled.")

        return GenerateResult(
            text=str(job.get("result") or ""),
            raw={"job_id": job_id, "status": status, "tier": tier},
            tokens_sent=None,
            tokens_received=None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if not self.token:
            missing.append("env:CYBERDINER token (runtime.adapters.cyberdiner.token)")
        if not self.expo_url:
            missing.append(
                "env:CYBERDINER expo_url (runtime.adapters.cyberdiner.expo_url)"
            )
            return CheckResult(ok=False, missing=missing)

        req = urllib.request.Request(
            self.expo_url.rstrip("/") + "/beta/jobs", method="GET"
        )
        req.add_header("Authorization", f"Bearer {self.token}")
        try:
            with urllib.request.urlopen(req, timeout=2):
                pass
        except urllib.error.HTTPError:
            # Any HTTP response (even 401/404/405) means the host answered —
            # only connection-level failures count as unreachable.
            pass
        except Exception:
            missing.append(f"host:{self.expo_url}")

        return CheckResult(ok=not missing, missing=missing)
