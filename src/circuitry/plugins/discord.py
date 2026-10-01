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

discord.py's ``SyncWebhook.send`` has no timeout parameter of its own, so
the effect's ``timeout_seconds`` is enforced by running it on a background
thread and giving up (raising ``TimeoutError``) if it hasn't returned by
the deadline. The thread is daemonic and not joined further, so a send
that's genuinely stuck doesn't block the run — but it also means the
underlying request isn't actually cancelled, just abandoned.
"""

from __future__ import annotations

import importlib.util
import queue
import threading
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, TypeVar

from ..preflight import CheckResult
from .base import ToolResult, _as_bool

_T = TypeVar("_T")


def _run_with_deadline(fn: Callable[[], _T], *, timeout_seconds: float, label: str) -> _T:
    """Run *fn* on a daemon thread and raise ``TimeoutError`` if it hasn't
    finished within *timeout_seconds*. Re-raises whatever *fn* raised,
    unchanged, when it finishes in time.
    """
    outcome: queue.SimpleQueue[tuple[str, Any]] = queue.SimpleQueue()

    def worker() -> None:
        try:
            outcome.put(("ok", fn()))
        except BaseException as exc:
            outcome.put(("error", exc))

    thread = threading.Thread(target=worker, daemon=True)
    thread.start()
    thread.join(timeout_seconds)
    if thread.is_alive():
        raise TimeoutError(f"{label} exceeded timeout of {timeout_seconds}s")

    status, payload = outcome.get()
    if status == "error":
        raise payload
    return payload  # type: ignore[no-any-return]


@dataclass(frozen=True)
class DiscordPlugin:
    name: str = "discord"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
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
        if _as_bool(params.get("tts")):
            kwargs["tts"] = True

        message = _run_with_deadline(
            lambda: webhook.send(**kwargs, wait=True),
            timeout_seconds=timeout_seconds,
            label="discord: webhook.send",
        )
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
