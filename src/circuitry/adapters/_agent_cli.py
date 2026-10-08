"""What the ``pi`` and ``claude_code`` adapters share on top of
:mod:`circuitry.agent_cli`: retry classification and the preflight check."""

from __future__ import annotations

import shutil
from collections.abc import Iterator
from contextlib import contextmanager

from ..agent_cli import AgentCliError, AgentCliResult, AgentCliTimeout
from ..preflight import CheckResult
from ._retry import AdapterCallError, RetryInfo
from .base import GenerateOptions, GenerateResult, ignored_options_warning


@contextmanager
def agent_cli_call_errors() -> Iterator[None]:
    """Re-raise an :class:`AgentCliError` as an :class:`AdapterCallError`:
    a timeout is retryable, any other CLI failure (a missing binary, an auth
    or version error) is not."""
    try:
        yield
    except AgentCliTimeout as exc:
        raise AdapterCallError(str(exc), retry_info=RetryInfo(retryable=True)) from exc
    except AgentCliError as exc:
        raise AdapterCallError(str(exc)) from exc


def generate_result(
    adapter_name: str, result: AgentCliResult, options: GenerateOptions | None
) -> GenerateResult:
    return GenerateResult(
        text=result.text,
        raw=result.raw,
        tokens_sent=result.tokens_sent,
        tokens_received=result.tokens_received,
        finish_reason=result.finish_reason,
        warnings=ignored_options_warning(adapter_name, options),
        cost_usd=result.cost_usd,
    )


def check_binary(adapter_name: str, binary: str) -> CheckResult:
    if shutil.which(binary) is None:
        return CheckResult(
            ok=False,
            missing=[f"binary:{binary}"],
            message=(
                f"{binary} is not on PATH: install it and log in, or set "
                f"runtime.adapters.{adapter_name}.binary."
            ),
        )
    return CheckResult(ok=True, missing=[])
