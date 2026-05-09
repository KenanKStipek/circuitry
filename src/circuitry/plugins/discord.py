"""Discord tool plugin via discord.py's SyncWebhook.

Optional dep: ``discord.py``. Install with ``pip install circuitry-cof[discord]``.

The synchronous webhook interface is the simplest non-bot integration —
it doesn't require maintaining a running gateway connection. For full
bot interactions (slash commands, reactions, voice), the orchestration
should run a separate discord.py bot process and communicate with it
out-of-band.

Params:
  - ``webhook_url`` (required, str): the channel's webhook URL.
  - ``content`` (required, str): message text.
  - ``username`` (optional, str): override the webhook's display name.
  - ``avatar_url`` (optional, str).
  - ``tts`` (optional, bool, default False).
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class DiscordPlugin:
    name: str = "discord"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        try:
            import discord  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "discord: discord.py not installed. "
                "Install with: pip install discord.py"
            ) from exc

        url = params.get("webhook_url")
        content = params.get("content")
        if not isinstance(url, str) or not url.strip():
            raise ValueError("discord requires params['webhook_url'].")
        if not isinstance(content, str):
            raise ValueError("discord requires params['content'].")

        webhook = discord.SyncWebhook.from_url(url.strip())
        kwargs: dict[str, Any] = {"content": content}
        if isinstance(params.get("username"), str):
            kwargs["username"] = params["username"]
        if isinstance(params.get("avatar_url"), str):
            kwargs["avatar_url"] = params["avatar_url"]
        if params.get("tts"):
            kwargs["tts"] = True

        message = webhook.send(**kwargs, wait=True)
        return ToolResult(
            value={
                "id": str(getattr(message, "id", "")),
                "channel_id": str(getattr(message, "channel_id", "")),
            },
            raw={"username": kwargs.get("username")},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("discord") is None:
            return CheckResult(
                ok=False,
                missing=["library:discord.py"],
                message="pip install discord.py",
            )
        return CheckResult(ok=True, missing=[])
