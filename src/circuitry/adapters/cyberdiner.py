"""Adapter for CyberDiner — a job-queue LLM broker.

CyberDiner's expo API is a job queue: a prompt is submitted as a job and
the result is fetched by polling — expo has no single blocking
completion endpoint. This adapter hides that behind circuitry's
synchronous ``generate()``: submit, poll, block until the job completes,
then return the text. That is exactly what cookd's own ``ask`` command
does (``apps/cook/cookd/src/commands/ask.rs`` in
KenanKStipek/CyberDiner), using the same two endpoints: ``POST
{expo_url}/beta/jobs`` with ``{prompt, tierName}`` and a bearer token
returns a ``jobId``; ``GET {expo_url}/beta/jobs/{jobId}`` is polled
(default every 500ms) until the job leaves the in-flight states
(``pending``/``assigned``/``running``) and reaches a terminal one
(``complete``/``completed``/``failed``/``cancelled``). No asyncio, no
threads — a prompt effect simply blocks like it does on every other
adapter.

Wire format — ground truth is ``apps/expo/src/models/job.rs`` in
KenanKStipek/CyberDiner, which carries ``#[serde(rename_all =
"camelCase")]`` and returns every job inside an ``ApiEnvelope``:

* request:  ``{"prompt": ..., "tierName": ...}`` (expo also accepts an
  optional ``priority`` of ``normal``/``fast``; the adapter omits it —
  a future ``runtime.adapters.cyberdiner.priority`` config knob)
* response: ``{"data": {"jobId", "status", "tierName", "priority",
  "prompt", "createdAt", "result", "tokensProcessed", "durationMs",
  "completedAt", "assignedAt", "errorCode", "errorMessage"}}`` — for
  both the create call and every poll.

Two spellings of terminal success are accepted: cookd's client checks
``complete`` while expo's ``ReportResultRequest`` writes ``completed``.
Accepting both is deliberate defensiveness, not indecision.

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

#: Terminal success. Both spellings are real: cookd's client polls for
#: ``complete``, expo's ``ReportResultRequest`` writes ``completed``.
_SUCCESS_STATUSES = frozenset({"complete", "completed"})
_TERMINAL_STATUSES = _SUCCESS_STATUSES | frozenset({"failed", "cancelled"})

#: Tier names expo seeds a deployment with. Suggestions only — expo's
#: ``tier_service`` is the authority, a deployment can define others, and
#: ``valid_tiers`` in config supersedes this list when set.
SEED_TIERS: tuple[str, ...] = (
    "cheap",
    "fast-cheap",
    "fast",
    "good-cheap",
    "good",
    "good-fast",
    "alpha",
)


def _resolve_tier(
    model: str, default_tier: str, valid_tiers: tuple[str, ...] = ()
) -> str:
    """Map an orchestration ``model:`` onto a CyberDiner tier name.

    The tier vocabulary belongs to the network, not to this client: expo's
    ``tier_service::validate_tier`` is the authority, and its 400 already
    surfaces through the adapter's actionable HTTP error path. So any
    non-empty tier passes through untouched. Set ``valid_tiers`` in adapter
    config to opt into client-side validation against a known list.
    """
    tier = (model or "").strip() or (default_tier or "").strip()
    if not tier:
        raise ValueError(
            "cyberdiner: no tier resolved. Pin `model:` in the orchestration "
            "or set runtime.adapters.cyberdiner.default_tier."
        )
    if valid_tiers and tier not in valid_tiers:
        valid = ", ".join(valid_tiers)
        raise ValueError(
            f"cyberdiner: unknown tier {tier!r}. Configured "
            f"runtime.adapters.cyberdiner.valid_tiers: {valid}."
        )
    return tier


def _unwrap(body: dict[str, Any], url: str) -> dict[str, Any]:
    """Peel expo's ``ApiEnvelope`` off a job response.

    Every expo job route answers ``{"data": {...}}``. A response without
    that envelope means we are not talking to the API we think we are —
    say so, rather than letting a ``.get()`` chain silently produce an
    empty completion.
    """
    data = body.get("data")
    if not isinstance(data, dict):
        raise RuntimeError(
            f"cyberdiner: response from {url} is not a CyberDiner job envelope — "
            "expected a top-level `data` object (expo wraps every job in "
            f"ApiEnvelope). Got: {json.dumps(body)[:200]}"
        )
    return data


def _as_int(value: Any) -> int | None:
    """Coerce a JSON number to ``int``, or ``None`` if it isn't one."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return int(value)


@dataclass(frozen=True)
class CyberdinerAdapter:
    name: str = "cyberdiner"
    expo_url: str = ""
    token: str = ""
    default_tier: str = "cheap"
    # Empty = pass-through: the network validates tier names. Populate it
    # (from config) only to fail fast against a known-good list.
    valid_tiers: tuple[str, ...] = ()
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

        tier = _resolve_tier(model, self.default_tier, tuple(self.valid_tiers))
        base = self.expo_url.rstrip("/")
        req_timeout = max(0.001, min(float(self.timeout_seconds), float(timeout_seconds)))
        deadline = time.monotonic() + timeout_seconds

        submit_url = f"{base}/beta/jobs"
        job = _unwrap(
            self._request(
                method="POST",
                url=submit_url,
                payload={"prompt": prompt, "tierName": tier},
                timeout=req_timeout,
            ),
            submit_url,
        )
        job_id = job.get("jobId")
        if not job_id:
            raise RuntimeError(f"cyberdiner: submit response missing jobId: {job!r}")

        poll_url = f"{base}/beta/jobs/{job_id}"
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
            job = _unwrap(
                self._request(
                    method="GET",
                    url=poll_url,
                    payload=None,
                    timeout=req_timeout,
                ),
                poll_url,
            )
            status = job.get("status")

        if status == "failed":
            error_message = job.get("errorMessage") or "unknown error"
            raise RuntimeError(f"cyberdiner: job {job_id} failed: {error_message}")
        if status == "cancelled":
            raise RuntimeError(f"cyberdiner: job {job_id} was cancelled.")

        return GenerateResult(
            text=str(job.get("result") or ""),
            raw={
                "jobId": job_id,
                "status": status,
                "tierName": tier,
                "durationMs": job.get("durationMs"),
                "data": job,
            },
            tokens_sent=None,
            # expo reports one `tokensProcessed` counter per job, not a
            # prompt/completion split, so it lands on tokens_received as an
            # approximation: it includes the prompt's tokens too.
            tokens_received=_as_int(job.get("tokensProcessed")),
        )

    def list_models(self) -> list[str]:
        """Tier names to offer as ``model:`` values.

        The configured ``valid_tiers`` when set — that is this
        deployment's own vocabulary — else the seed names. No network
        call: expo has no tier-listing endpoint, and a picker should not
        need a token to show suggestions.
        """
        return list(self.valid_tiers) if self.valid_tiers else list(SEED_TIERS)

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
