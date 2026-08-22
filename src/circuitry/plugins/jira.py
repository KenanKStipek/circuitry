"""Jira tool plugin via atlassian-python-api.

Optional dep: ``atlassian-python-api``. Install with
``pip install circuitry-cof[jira]``.

Auth (Atlassian Cloud):
  - ``JIRA_URL`` — your Atlassian site (``https://acme.atlassian.net``).
  - ``JIRA_USER`` — email.
  - ``JIRA_TOKEN`` — API token from id.atlassian.com/manage-profile/security.

Params:
  - ``mode``: ``"get_issue" | "search" | "create_issue" | "transition"``.
  - ``key`` (get_issue / transition): issue key (``"PROJ-123"``).
  - ``jql`` (search): JQL query.
  - ``fields`` (search, optional): list of field names.
  - ``project``, ``summary``, ``description``, ``issuetype`` (create_issue).
  - ``status`` (transition): target status name.
"""

from __future__ import annotations

import importlib.util
import os
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _jira_client(timeout_seconds: int) -> Any:
    """Build a Jira client using Atlassian Cloud env vars."""
    from atlassian import Jira  # type: ignore[import-not-found]

    url = os.environ.get("JIRA_URL", "").rstrip("/")
    user = os.environ.get("JIRA_USER", "")
    token = os.environ.get("JIRA_TOKEN", "")
    if not (url and user and token):
        raise RuntimeError(
            "jira: JIRA_URL, JIRA_USER, JIRA_TOKEN must all be set."
        )
    return Jira(url=url, username=user, password=token, cloud=True, timeout=timeout_seconds)


@dataclass(frozen=True)
class JiraPlugin:
    name: str = "jira"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        try:
            import atlassian  # type: ignore[import-not-found]
            del atlassian  # presence check only — actual import below
        except ImportError as exc:
            raise RuntimeError(
                "jira: atlassian-python-api not installed. "
                "Install with: pip install atlassian-python-api"
            ) from exc

        client = _jira_client(int(timeout_seconds))
        mode = str(params.get("mode") or "get_issue").lower()

        if mode == "get_issue":
            key = params.get("key")
            if not isinstance(key, str) or not key:
                raise ValueError("jira get_issue requires params['key'].")
            value: Any = client.issue(key)
        elif mode == "search":
            jql = params.get("jql")
            if not isinstance(jql, str) or not jql.strip():
                raise ValueError("jira search requires params['jql'].")
            fields = params.get("fields")
            value = client.jql(jql, fields=fields)
        elif mode == "create_issue":
            project = params.get("project")
            summary = params.get("summary")
            if not isinstance(project, str) or not project:
                raise ValueError("jira create_issue requires params['project'].")
            if not isinstance(summary, str) or not summary:
                raise ValueError("jira create_issue requires params['summary'].")
            issue_fields: dict[str, Any] = {
                "project": {"key": project},
                "summary": summary,
                "issuetype": {"name": str(params.get("issuetype") or "Task")},
            }
            if isinstance(params.get("description"), str):
                issue_fields["description"] = params["description"]
            value = client.create_issue(fields=issue_fields)
        elif mode == "transition":
            key = params.get("key")
            status = params.get("status")
            if not isinstance(key, str) or not key:
                raise ValueError("jira transition requires params['key'].")
            if not isinstance(status, str) or not status:
                raise ValueError("jira transition requires params['status'].")
            value = client.set_issue_status(key, status)
        else:
            raise ValueError(f"jira: unknown mode {mode!r}")

        return ToolResult(
            value=value, raw={"mode": mode},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if importlib.util.find_spec("atlassian") is None:
            missing.append("library:atlassian-python-api")
        missing.extend(
            f"env:{env}"
            for env in ("JIRA_URL", "JIRA_USER", "JIRA_TOKEN")
            if not os.environ.get(env)
        )
        if missing:
            return CheckResult(ok=False, missing=missing)
        return CheckResult(ok=True, missing=[])
