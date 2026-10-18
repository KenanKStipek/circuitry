"""``type: report`` — tells watchers (``--events``, ``osi``) what a run is
doing, with values picked from the shadow state (#605).

Modelled on :mod:`circuitry.core.yield_effect`: no model call, no retries,
no ``on_error`` — but unlike a yield, a report never fails the run even
when its own rendering raises, because it exists for watching, not doing.
It fires a dedicated :meth:`circuitry.core.store.Store.fire_report` hook,
never ``effect_start``/``effect_complete``, so it never appears as a
start/end event and never counts toward ``runtime.last_run.totals.effects_run``.
"""

from __future__ import annotations

import copy
import logging
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Literal

from .prompt_compose import (
    declared_prompts,
    known_effect_names,
    render_with_composition,
)
from .store import Store
from .use import _resolve_reference

logger = logging.getLogger(__name__)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


#: A report ``data`` leaf's by-reference form: exactly ``{from: <path>}`` —
#: a report never fails on an unresolved path, so there is nothing a
#: ``default:`` would be for, and no ``as:`` (#605; contrast a tool
#: effect's richer ``{from:}`` grammar, #234/#566).
def report_data_reference(value: Any) -> str | None:
    if isinstance(value, dict) and set(value) == {"from"}:
        path = value["from"]
        if isinstance(path, str):
            return path.strip()
    return None


@dataclass(frozen=True)
class ReportDefinition:
    """A ``report`` effect: render ``message`` (+ optional ``data``) and
    publish it to every watcher — see the module docstring.
    """

    message: str
    name: str | None = None
    data: dict[str, Any] | None = None
    level: Literal["info", "warning"] = "info"
    description: str | None = None

    # False = skip execution and write a disabled node (see core.disabled),
    # only when named — an unnamed report has no node to write.
    enabled: bool = True


class ReportRuntime:
    """Executes a :class:`ReportDefinition` against ``store``/``ctx``.

    Never raises: a failure anywhere in rendering is caught, logged once,
    and the report still publishes (with an empty message/no data) rather
    than failing the run — see the module docstring.
    """

    def __init__(
        self,
        definition: ReportDefinition,
        *,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
    ) -> None:
        self.defn = definition
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        path = store.effect_path(self.defn.name) if self.defn.name else store.own_path

        message = ""
        data: Any = None
        try:
            message = render_with_composition(
                self.defn.message,
                ctx,
                declared=declared_prompts(self.runtime_config),
                known_effect_names=known_effect_names(self.runtime_config),
                label=f"report '{self.defn.name}' message"
                if self.defn.name
                else f"report at '{path}' message",
                escape=False,
            )
            if self.defn.data is not None:
                data = self._resolve_data(self.defn.data, ctx, path=path, field="data")
        except Exception as exc:
            logger.warning(
                "report at '%s' failed to render; publishing it with no "
                "message or data: %s",
                path,
                exc,
            )
            message = ""
            data = None

        if self.defn.name:
            node = store.ensure_dict(self.defn.name)
            node["value"] = {"message": message, "level": self.defn.level, "data": data}
            meta = node.get("meta")
            if not isinstance(meta, dict):
                meta = {}
                node["meta"] = meta
            now = _now_iso()
            meta["created_at"] = now
            meta["completed_at"] = now
            meta["dry_run"] = self.dry_run

        try:
            store.fire_report(self.defn.name, level=self.defn.level, message=message, data=data)
        except Exception as exc:
            # A report must never fail the run (#605) -- not even when an
            # observer it never chose (the console, --events, a plugin)
            # raises on it, e.g. unescaped Rich markup in the message.
            logger.warning("report at '%s' observer raised; continuing: %s", path, exc)

    def _resolve_data(self, value: Any, ctx: dict[str, Any], *, path: str, field: str) -> Any:
        ref_path = report_data_reference(value)
        if ref_path is not None:
            resolved = _resolve_reference(ctx, ref_path)
            if resolved is None:
                logger.warning(
                    "report at '%s' data '%s': '{from: %s}' did not resolve "
                    "to a value; using null.",
                    path,
                    field,
                    ref_path,
                )
                return None
            return copy.deepcopy(resolved)
        if isinstance(value, str):
            return render_with_composition(
                value,
                ctx,
                declared=declared_prompts(self.runtime_config),
                known_effect_names=known_effect_names(self.runtime_config),
                label=f"report data '{field}'",
                escape=False,
            )
        if isinstance(value, dict):
            return {
                key: self._resolve_data(item, ctx, path=path, field=f"{field}.{key}")
                for key, item in value.items()
            }
        if isinstance(value, list):
            return [
                self._resolve_data(item, ctx, path=path, field=f"{field}[{index}]")
                for index, item in enumerate(value)
            ]
        return value
