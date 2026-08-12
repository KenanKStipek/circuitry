from __future__ import annotations

import re
from collections.abc import Callable
from dataclasses import dataclass
from re import Pattern
from typing import Any, ClassVar

from ..preflight import CheckResult
from .base import GenerateResult


class RunCancelled(BaseException):
    """
    Raised by a request_handler to abort the orchestration cleanly.

    Extends ``BaseException`` (not ``Exception``) so the prompt runtime's
    generic ``except Exception`` adapter-fallback wrapper does not swallow
    it. Cancellation must propagate up the runtime stack unchanged so the
    worker thread observes it and marks the run CANCELLED.
    """


@dataclass(frozen=True)
class HostPromptRequest:
    prompt: str
    model: str
    timeout_seconds: int = 120


_CLAUDE_MODEL_PATTERN: Pattern[str] = re.compile(r"^(claude(-.+)?|)$")


@dataclass(frozen=True)
class HostClaudeAdapter:
    """
    Adapter that hands prompts to a host LLM (e.g., the Claude session driving
    circuitry-mcp). Instead of an HTTP call, `generate()` invokes an injected
    `request_handler(HostPromptRequest) -> str` callback. The MCP server wires
    this callback to per-prompt blocking queues so the host's response routes
    back to the correct paused branch.

    Model-pin policy:
      - Claude-family pins (or empty) are always accepted; the host generates
        with whatever Claude model is driving it.
      - Non-Claude pins are rejected by default — silently honoring a pinned
        `gpt-4o` would lie about which model produced the value.
      - `override_model=True` (set per-run, e.g. via the MCP server) ignores
        the orchestration's pin entirely and runs the prompt through Claude
        regardless. Useful for testing an orchestration end-to-end via Claude
        before deploying it with its real backend. The original pin is
        preserved in `raw["overridden_from"]` for traceability; `raw["model"]`
        reflects what actually generated the value (`override_to`, or empty
        meaning "whatever the host is running").
    """

    request_handler: Callable[[HostPromptRequest], str]
    name: str = "host_claude"
    default_model: str = ""
    override_model: bool = False
    override_to: str = ""

    _claude_model_pattern: ClassVar[Pattern[str]] = _CLAUDE_MODEL_PATTERN

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        requested_model = model or self.default_model
        is_claude = bool(self._claude_model_pattern.match(requested_model or ""))

        if is_claude:
            effective_model = requested_model
            overridden_from: str | None = None
        elif self.override_model:
            # Strip the orchestration's non-Claude pin and run through Claude.
            effective_model = self.override_to
            overridden_from = requested_model
        else:
            raise ValueError(
                f"host_claude only accepts Claude-family models or empty; "
                f"got {requested_model!r}. Either omit `model:`, pin a "
                "claude-* value, or run with override_model=True "
                "(MCP: run_orchestration(..., override_model=True))."
            )

        request = HostPromptRequest(
            prompt=prompt,
            model=effective_model,
            timeout_seconds=timeout_seconds,
        )

        text = self.request_handler(request)

        raw: dict[str, Any] = {"adapter": "host_claude", "model": effective_model}
        if overridden_from is not None:
            raw["overridden_from"] = overridden_from

        return GenerateResult(
            text=text if isinstance(text, str) else str(text),
            raw=raw,
            tokens_sent=None,
            tokens_received=None,
        )

    def check(self) -> CheckResult:
        # The MCP server injects the request_handler; the adapter has no
        # external dependencies of its own. A CheckResult.ok=True here just
        # confirms the dataclass instance is wired.
        return CheckResult(
            ok=True,
            missing=[],
            message="host_claude is driven by the host LLM session (MCP).",
        )
