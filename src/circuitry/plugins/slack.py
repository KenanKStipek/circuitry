"""Slack tool plugin via slack-sdk.

Optional dep: ``slack-sdk``. Install with ``pip install circuitry-cof[slack]``.

Auth: ``SLACK_BOT_TOKEN`` (xoxb-...) — bot tokens have stable scopes;
xoxp user tokens also work.

Params:
  - ``mode``: ``"post_message"`` (default) | ``"list_channels"`` |
    ``"history"``.
  - ``channel`` (post_message / history, required): channel ID or
    name (e.g. ``"#general"`` or ``"C0123456"``).
  - ``text`` (post_message, required, str).
  - ``thread_ts`` (post_message, optional, str): post as reply.
  - ``blocks`` (post_message, optional, list[dict]): Block Kit blocks.
  - ``limit`` (history / list_channels, optional, int).
"""

from __future__ import annotations

import importlib.util
import os
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class SlackPlugin:
    name: str = "slack"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        try:
            from slack_sdk import WebClient  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "slack: slack-sdk not installed. "
                "Install with: pip install slack-sdk"
            ) from exc

        token = os.environ.get("SLACK_BOT_TOKEN", "")
        if not token:
            raise RuntimeError("slack: SLACK_BOT_TOKEN not set.")
        client = WebClient(token=token, timeout=int(timeout_seconds))

        mode = str(params.get("mode") or "post_message").lower()
        if mode == "post_message":
            channel = params.get("channel")
            text = params.get("text")
            if not isinstance(channel, str) or not channel:
                raise ValueError("slack post_message requires params['channel'].")
            if not isinstance(text, str):
                raise ValueError("slack post_message requires params['text'].")
            kwargs: dict[str, Any] = {"channel": channel, "text": text}
            if isinstance(params.get("thread_ts"), str):
                kwargs["thread_ts"] = params["thread_ts"]
            if isinstance(params.get("blocks"), list):
                kwargs["blocks"] = params["blocks"]
            response = client.chat_postMessage(**kwargs)
            data = dict(response.data) if hasattr(response, "data") else dict(response)
            return ToolResult(
                value={"ts": data.get("ts"), "channel": data.get("channel")},
                raw=data, stdout=None, stderr=None, exit_code=None,
            )

        if mode == "list_channels":
            limit = int(params.get("limit") or 100)
            response = client.conversations_list(limit=limit)
            data = dict(response.data) if hasattr(response, "data") else dict(response)
            return ToolResult(
                value=data.get("channels") or [],
                raw={"count": len(data.get("channels") or [])},
                stdout=None, stderr=None, exit_code=None,
            )

        if mode == "history":
            channel = params.get("channel")
            if not isinstance(channel, str) or not channel:
                raise ValueError("slack history requires params['channel'].")
            limit = int(params.get("limit") or 50)
            response = client.conversations_history(channel=channel, limit=limit)
            data = dict(response.data) if hasattr(response, "data") else dict(response)
            return ToolResult(
                value=data.get("messages") or [],
                raw={"count": len(data.get("messages") or [])},
                stdout=None, stderr=None, exit_code=None,
            )

        raise ValueError(f"slack: unknown mode {mode!r}")

    def check(self) -> CheckResult:
        missing: list[str] = []
        if importlib.util.find_spec("slack_sdk") is None:
            missing.append("library:slack-sdk")
        if not os.environ.get("SLACK_BOT_TOKEN"):
            missing.append("env:SLACK_BOT_TOKEN")
        if missing:
            return CheckResult(ok=False, missing=missing)
        return CheckResult(ok=True, missing=[])
