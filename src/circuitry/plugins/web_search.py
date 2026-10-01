"""Web search tool plugin.

Hits DuckDuckGo's Instant Answer API by default — no key required, but
results are limited to direct answers and disambiguation entries (not
full SERP). Override ``base_url`` and ``query_param`` to point at a
different search service when you have an API endpoint that returns
JSON.

Params:
  - ``query`` (required, str)
  - ``base_url`` (optional, default DuckDuckGo IA)
  - ``query_param`` (optional, default ``"q"``)
  - ``extra_params`` (optional, dict[str,str]): merged onto the request.

Returns parsed JSON when the response advertises JSON content type,
else the raw text.
"""

from __future__ import annotations

import json as _json
import shutil
import urllib.parse
from dataclasses import dataclass
from typing import Any

from ..curl_support import curl_failure_message, run_curl
from ..preflight import CheckResult
from .base import ToolResult

_DUCKDUCKGO_IA = "https://api.duckduckgo.com/"


@dataclass(frozen=True)
class WebSearchPlugin:
    name: str = "web_search"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        query = params.get("query")
        if not isinstance(query, str) or not query.strip():
            raise ValueError("web_search: params['query'] required.")
        base_url = params.get("base_url") or _DUCKDUCKGO_IA
        query_param = str(params.get("query_param") or "q")
        extra = params.get("extra_params") or {}
        if not isinstance(extra, dict):
            raise ValueError(
                "web_search: params['extra_params'] must be a dict."
            )

        merged: dict[str, str] = {query_param: query}
        # DuckDuckGo IA needs format=json.
        if "duckduckgo.com" in str(base_url) and "format" not in extra:
            merged["format"] = "json"
            merged.setdefault("no_html", "1")
            merged.setdefault("skip_disambig", "1")
        for k, v in extra.items():
            merged[str(k)] = str(v)

        sep = "&" if "?" in str(base_url) else "?"
        url = f"{base_url}{sep}{urllib.parse.urlencode(merged, doseq=True)}"

        if shutil.which("curl") is None:
            raise RuntimeError("web_search: curl not on PATH.")

        proc = run_curl(url=url, timeout_seconds=timeout_seconds)
        if proc.returncode != 0:
            raise RuntimeError(
                curl_failure_message(
                    source="web_search",
                    url=url,
                    returncode=proc.returncode,
                    stdout=proc.stdout,
                    stderr=proc.stderr,
                )
            )

        body = proc.stdout
        try:
            value: Any = _json.loads(body) if body.strip() else None
        except _json.JSONDecodeError:
            value = body
        return ToolResult(
            value=value,
            raw={"url": url, "body": body},
            stdout=None, stderr=None, exit_code=proc.returncode,
        )

    def check(self) -> CheckResult:
        if shutil.which("curl") is None:
            return CheckResult(ok=False, missing=["binary:curl"])
        return CheckResult(ok=True, missing=[])
