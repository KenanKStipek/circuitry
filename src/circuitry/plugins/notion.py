"""Notion tool plugin via notion-client.

Optional dep: ``notion-client``. Install with
``pip install circuitry-cof[notion]``.

Auth: ``NOTION_TOKEN`` from a Notion integration
(https://www.notion.so/my-integrations).

Params:
  - ``mode``: ``"query_database" | "get_page" | "create_page" |
    "search"``.
  - ``database_id`` (query_database).
  - ``page_id`` (get_page).
  - ``parent_id`` (create_page): parent database id.
  - ``properties`` (create_page): properties dict in Notion's format.
  - ``query`` (search, optional): substring filter.
  - ``filter`` (query_database, optional): filter object.
  - ``sorts`` (query_database, optional).
  - ``page_size`` (query_database / search, optional, default 100).
"""

from __future__ import annotations

import importlib.util
import os
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class NotionPlugin:
    name: str = "notion"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        try:
            from notion_client import Client  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "notion: notion-client not installed. "
                "Install with: pip install notion-client"
            ) from exc

        token = os.environ.get("NOTION_TOKEN", "")
        if not token:
            raise RuntimeError("notion: NOTION_TOKEN not set.")
        client = Client(auth=token, timeout_ms=int(timeout_seconds) * 1000)

        mode = str(params.get("mode") or "query_database").lower()

        if mode == "query_database":
            db_id = params.get("database_id")
            if not isinstance(db_id, str) or not db_id:
                raise ValueError("notion query_database requires params['database_id'].")
            kwargs: dict[str, Any] = {
                "database_id": db_id,
                "page_size": int(params.get("page_size") or 100),
            }
            if isinstance(params.get("filter"), dict):
                kwargs["filter"] = params["filter"]
            if isinstance(params.get("sorts"), list):
                kwargs["sorts"] = params["sorts"]
            response = client.databases.query(**kwargs)
            value: Any = response.get("results") or []

        elif mode == "get_page":
            page_id = params.get("page_id")
            if not isinstance(page_id, str) or not page_id:
                raise ValueError("notion get_page requires params['page_id'].")
            value = client.pages.retrieve(page_id=page_id)

        elif mode == "create_page":
            parent_id = params.get("parent_id")
            properties = params.get("properties")
            if not isinstance(parent_id, str) or not parent_id:
                raise ValueError("notion create_page requires params['parent_id'].")
            if not isinstance(properties, dict):
                raise ValueError("notion create_page requires params['properties'] as dict.")
            value = client.pages.create(
                parent={"database_id": parent_id}, properties=properties
            )

        elif mode == "search":
            kwargs = {"page_size": int(params.get("page_size") or 100)}
            if isinstance(params.get("query"), str):
                kwargs["query"] = params["query"]
            response = client.search(**kwargs)
            value = response.get("results") or []

        else:
            raise ValueError(f"notion: unknown mode {mode!r}")

        return ToolResult(
            value=value, raw={"mode": mode},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if importlib.util.find_spec("notion_client") is None:
            missing.append("library:notion-client")
        if not os.environ.get("NOTION_TOKEN"):
            missing.append("env:NOTION_TOKEN")
        if missing:
            return CheckResult(ok=False, missing=missing)
        return CheckResult(ok=True, missing=[])
