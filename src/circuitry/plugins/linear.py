"""Linear (linear.app) GraphQL tool plugin.

Optional dep: ``requests``. Install with ``pip install circuitry-cof[linear]``.

Linear has no first-party Python SDK; this plugin issues GraphQL
requests directly against ``api.linear.app/graphql``.

Auth: ``LINEAR_API_KEY`` (personal access token from
https://linear.app/settings/api).

Params:
  - ``mode`` (optional, default ``"query"``): ``"query"`` runs an
    arbitrary GraphQL query; ``"list_issues"`` is a convenience shortcut.
  - ``query`` (query mode, str): GraphQL document.
  - ``variables`` (query mode, optional dict).
  - ``team`` (list_issues, optional str): team key (e.g. ``"ENG"``).
  - ``state`` (list_issues, optional str): state name (e.g. ``"In Progress"``).
  - ``limit`` (list_issues, int, default 50).
"""

from __future__ import annotations

import importlib.util
import os
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult

_LINEAR_ENDPOINT = "https://api.linear.app/graphql"

_LIST_ISSUES_QUERY = """
query ListIssues($first: Int!, $filter: IssueFilter) {
  issues(first: $first, filter: $filter) {
    nodes {
      id identifier title state { name }
      team { key name } assignee { name }
      priority createdAt updatedAt url
    }
  }
}
"""


@dataclass(frozen=True)
class LinearPlugin:
    name: str = "linear"

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
                "linear: requests not installed. "
                "Install with: pip install requests"
            ) from exc

        api_key = os.environ.get("LINEAR_API_KEY", "")
        if not api_key:
            raise RuntimeError("linear: LINEAR_API_KEY not set.")

        mode = str(params.get("mode") or "query").lower()
        query: str
        variables: Any
        if mode == "list_issues":
            filter_obj: dict[str, Any] = {}
            if isinstance(params.get("team"), str):
                filter_obj["team"] = {"key": {"eq": params["team"]}}
            if isinstance(params.get("state"), str):
                filter_obj["state"] = {"name": {"eq": params["state"]}}
            variables = {
                "first": int(params.get("limit") or 50),
                "filter": filter_obj or None,
            }
            query = _LIST_ISSUES_QUERY
        elif mode == "query":
            raw_query = params.get("query")
            if not isinstance(raw_query, str) or not raw_query.strip():
                raise ValueError("linear query mode requires params['query'].")
            query = raw_query
            variables = params.get("variables")
        else:
            raise ValueError(f"linear: unknown mode {mode!r}")

        try:
            resp = requests.post(
                _LINEAR_ENDPOINT,
                headers={
                    "Authorization": api_key,
                    "Content-Type": "application/json",
                },
                json={"query": query, "variables": variables},
                timeout=int(timeout_seconds),
            )
        except requests.exceptions.RequestException as exc:
            raise RuntimeError(f"linear request failed: {exc}") from exc

        if resp.status_code >= 400:
            raise RuntimeError(
                f"linear HTTP {resp.status_code}: {resp.text[:500]}"
            )
        body = resp.json()
        if isinstance(body, dict) and body.get("errors"):
            raise RuntimeError(f"linear GraphQL errors: {body['errors']}")
        data = (body or {}).get("data")

        if mode == "list_issues":
            value: Any = (
                ((data or {}).get("issues") or {}).get("nodes") or []
            )
        else:
            value = data
        return ToolResult(
            value=value,
            raw={"mode": mode, "status": resp.status_code},
            stdout=None, stderr=None, exit_code=resp.status_code,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if importlib.util.find_spec("requests") is None:
            missing.append("library:requests")
        if not os.environ.get("LINEAR_API_KEY"):
            missing.append("env:LINEAR_API_KEY")
        if missing:
            return CheckResult(ok=False, missing=missing)
        return CheckResult(ok=True, missing=[])
