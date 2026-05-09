"""GitHub API tool plugin via PyGithub.

Optional dep: ``PyGithub``. Install with ``pip install circuitry-cof[github]``.

Auth: ``GITHUB_TOKEN`` (PAT or fine-grained token). For unauthenticated
public-repo reads, the env var can be omitted (with reduced rate limits).

Params:
  - ``mode``: ``"get_repo"`` | ``"list_issues"`` | ``"create_issue"`` |
    ``"get_pull"`` | ``"list_pulls"``.
  - ``repo`` (required, str): ``"owner/name"``.
  - ``state`` (list_issues / list_pulls, optional, default ``"open"``):
    ``"open" | "closed" | "all"``.
  - ``limit`` (list modes, optional int).
  - ``title``, ``body``, ``labels`` (create_issue): standard GitHub fields.
  - ``number`` (get_pull, int).
"""

from __future__ import annotations

import importlib.util
import os
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


def _issue_to_dict(issue: Any) -> dict[str, Any]:
    return {
        "number": getattr(issue, "number", None),
        "title": getattr(issue, "title", ""),
        "state": getattr(issue, "state", ""),
        "url": getattr(issue, "html_url", ""),
        "user": getattr(getattr(issue, "user", None), "login", ""),
        "labels": [getattr(label, "name", "") for label in getattr(issue, "labels", []) or []],
        "created_at": str(getattr(issue, "created_at", "")),
    }


def _pr_to_dict(pr: Any) -> dict[str, Any]:
    return {
        "number": getattr(pr, "number", None),
        "title": getattr(pr, "title", ""),
        "state": getattr(pr, "state", ""),
        "url": getattr(pr, "html_url", ""),
        "user": getattr(getattr(pr, "user", None), "login", ""),
        "merged": bool(getattr(pr, "merged", False)),
        "draft": bool(getattr(pr, "draft", False)),
        "head": getattr(getattr(pr, "head", None), "ref", ""),
        "base": getattr(getattr(pr, "base", None), "ref", ""),
    }


@dataclass(frozen=True)
class GitHubPlugin:
    name: str = "github"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        # Validate the repo slug first so a malformed value yields a
        # clean ValueError even when PyGithub isn't installed.
        repo_slug = params.get("repo")
        if not isinstance(repo_slug, str) or "/" not in repo_slug:
            raise ValueError("github requires params['repo'] as 'owner/name'.")
        mode = str(params.get("mode") or "get_repo").lower()

        try:
            from github import Github  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "github: PyGithub not installed. "
                "Install with: pip install PyGithub"
            ) from exc

        token = os.environ.get("GITHUB_TOKEN", "")
        gh = Github(token, timeout=int(timeout_seconds)) if token else Github(timeout=int(timeout_seconds))
        repo = gh.get_repo(repo_slug.strip())

        if mode == "get_repo":
            value: Any = {
                "name": repo.name,
                "full_name": repo.full_name,
                "description": repo.description or "",
                "stars": repo.stargazers_count,
                "forks": repo.forks_count,
                "open_issues": repo.open_issues_count,
                "default_branch": repo.default_branch,
                "url": repo.html_url,
            }
        elif mode == "list_issues":
            state = str(params.get("state") or "open")
            limit = int(params.get("limit") or 50)
            issues = list(repo.get_issues(state=state)[:limit])
            value = [_issue_to_dict(i) for i in issues if not getattr(i, "pull_request", None)]
        elif mode == "create_issue":
            title = params.get("title")
            if not isinstance(title, str) or not title.strip():
                raise ValueError("github create_issue requires params['title'].")
            body = str(params.get("body") or "")
            labels = params.get("labels") or []
            issue = repo.create_issue(title=title, body=body, labels=list(labels))
            value = _issue_to_dict(issue)
        elif mode == "list_pulls":
            state = str(params.get("state") or "open")
            limit = int(params.get("limit") or 50)
            prs = list(repo.get_pulls(state=state)[:limit])
            value = [_pr_to_dict(p) for p in prs]
        elif mode == "get_pull":
            number = int(params.get("number") or 0)
            if number <= 0:
                raise ValueError("github get_pull requires params['number'].")
            value = _pr_to_dict(repo.get_pull(number))
        else:
            raise ValueError(f"github: unknown mode {mode!r}")

        return ToolResult(
            value=value, raw={"repo": repo_slug, "mode": mode},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("github") is None:
            return CheckResult(
                ok=False,
                missing=["library:PyGithub"],
                message="pip install PyGithub",
            )
        return CheckResult(ok=True, missing=[])
