"""Webhook POST tool plugin via requests.

Optional dep: ``requests``. Install with ``pip install circuitry-cof[webhook]``.

Why a separate plugin from ``http``? The http tool uses stdlib urllib
to keep circuitry dependency-free. Webhook callers typically want
features that requests provides cleanly: easy connection pooling, retry
on transient failures, multipart bodies, robust redirect handling.

Params:
  - ``url`` (required, str)
  - ``method`` (optional, default ``"POST"``)
  - ``json`` (optional): dict body — sent with Content-Type: application/json.
  - ``data`` (optional): form / raw body (mutually exclusive with json).
  - ``headers`` (optional, dict[str, str])
  - ``params`` (optional, dict[str, str]): query string params.
  - ``retries`` (optional, int, default 0): retry on 5xx and connection
    errors with exponential backoff.

Returns ``value`` = parsed JSON body if Content-Type is JSON, else text.
``exit_code`` = HTTP status; ``stderr`` carries the reason on non-2xx.
"""

from __future__ import annotations

import importlib.util
import time
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class WebhookPlugin:
    name: str = "webhook"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        try:
            import requests  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "webhook: requests not installed. "
                "Install with: pip install requests"
            ) from exc

        url = params.get("url")
        if not isinstance(url, str) or not url.strip():
            raise ValueError("webhook requires params['url'].")
        method = str(params.get("method") or "POST").upper()
        headers = dict(params.get("headers") or {})
        json_body = params.get("json")
        data_body = params.get("data")
        if json_body is not None and data_body is not None:
            raise ValueError(
                "webhook: pass either params['json'] or params['data'], not both."
            )
        query = params.get("params")
        retries = int(params.get("retries") or 0)

        last_error: Exception | None = None
        last_status: int | None = None
        last_text: str = ""
        last_headers: dict[str, str] = {}

        for attempt in range(retries + 1):
            try:
                resp = requests.request(
                    method=method,
                    url=url.strip(),
                    headers=headers,
                    json=json_body,
                    data=data_body,
                    params=query,
                    timeout=int(timeout_seconds),
                )
                last_status = int(resp.status_code)
                last_text = resp.text
                last_headers = {k.lower(): v for k, v in resp.headers.items()}
                if 500 <= resp.status_code < 600 and attempt < retries:
                    # Transient — back off and retry.
                    time.sleep(min(30.0, 2.0 ** attempt))
                    continue
                break
            except requests.exceptions.RequestException as exc:
                last_error = exc
                if attempt < retries:
                    time.sleep(min(30.0, 2.0 ** attempt))
                    continue
                raise RuntimeError(f"webhook request failed: {exc}") from exc

        if last_status is None:
            raise RuntimeError(
                f"webhook request failed after {retries + 1} attempts: {last_error}"
            )

        is_json = "json" in (last_headers.get("content-type") or "")
        if is_json and last_text:
            try:
                import json as _json
                value: Any = _json.loads(last_text)
            except ValueError:
                value = last_text
        else:
            value = last_text

        stderr = (
            None
            if 200 <= last_status < 300
            else f"HTTP {last_status}"
        )
        return ToolResult(
            value=value,
            raw={
                "url": url,
                "status": last_status,
                "headers": last_headers,
                "body": last_text,
            },
            stdout=None,
            stderr=stderr,
            exit_code=last_status,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("requests") is None:
            return CheckResult(
                ok=False,
                missing=["library:requests"],
                message="pip install requests",
            )
        return CheckResult(ok=True, missing=[])
