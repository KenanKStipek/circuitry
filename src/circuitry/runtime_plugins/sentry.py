"""Sentry runtime plugin via sentry-sdk.

Captures exceptions on run failure as Sentry events with the run's
``run_id`` and ``orchestration_path`` as tags. Effects add structured
breadcrumbs so the failure event includes the trail of completed
effects leading up to the error.

Optional dep: ``sentry-sdk``. Install with
``pip install circuitry-cof[sentry]``.

Auth: ``SENTRY_DSN`` env var (or ``runtime_plugins.sentry.dsn``).
"""

from __future__ import annotations

import importlib.util
import logging
import os
from typing import Any

from ..preflight import CheckResult

logger = logging.getLogger(__name__)


class SentryPlugin:
    name: str = "sentry"

    def __init__(self) -> None:
        self._sentry_sdk: Any = None
        self._dsn: str | None = None

    def _check_dep(self) -> tuple[bool, list[str]]:
        if importlib.util.find_spec("sentry_sdk") is None:
            return False, ["library:sentry-sdk"]
        if not (os.environ.get("SENTRY_DSN")):
            # Surface as missing only if there is no runtime config DSN
            # either; we can't tell at check() time without a context
            # so we report the env var as a candidate hint.
            return False, ["env:SENTRY_DSN"]
        return True, []

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        try:
            import sentry_sdk  # type: ignore[import-not-found]
        except ImportError:
            return
        self._sentry_sdk = sentry_sdk
        cfg = (
            (context.runtime_config or {})
            .get("runtime_plugins", {})
            .get("sentry", {})
        )
        dsn = (
            os.environ.get("SENTRY_DSN") or cfg.get("dsn")
        )
        if not dsn:
            logger.info("sentry: no DSN configured; plugin is a no-op.")
            return
        self._dsn = dsn
        # init() is idempotent — if the host already initialised
        # sentry, this just configures additional scopes.
        sentry_sdk.init(dsn=dsn)
        sentry_sdk.set_tag("circuitry.run_id", context.run_id)
        sentry_sdk.set_tag(
            "circuitry.orchestration_path", str(context.orchestration_path)
        )
        sentry_sdk.add_breadcrumb(
            category="circuitry",
            message=f"run_start {context.run_id}",
            level="info",
        )

    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None:
        del state, context
        if self._sentry_sdk is None:
            return
        meta = effect_result.get("meta") if isinstance(effect_result, dict) else {}
        meta = meta if isinstance(meta, dict) else {}
        error = meta.get("error")
        self._sentry_sdk.add_breadcrumb(
            category="circuitry.effect",
            message=f"effect_complete {effect_path}",
            level="error" if error else "info",
            data={"effect_path": effect_path, "error": error},
        )

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state, context
        if self._sentry_sdk is None:
            return
        self._sentry_sdk.add_breadcrumb(
            category="circuitry",
            message="run_success",
            level="info",
        )

    def on_run_failure(
        self, *, state: dict[str, Any], context: Any, error: str
    ) -> None:
        del state, context
        if self._sentry_sdk is None:
            return
        self._sentry_sdk.capture_message(
            f"circuitry run failed: {error}", level="error"
        )

    def check(self) -> CheckResult:
        ok, missing = self._check_dep()
        return CheckResult(ok=ok, missing=missing)


def plugin() -> SentryPlugin:
    return SentryPlugin()
