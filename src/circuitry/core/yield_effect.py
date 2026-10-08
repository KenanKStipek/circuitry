"""``type: yield`` — a prompt as a value, with no model call (#396).

Renders ``template`` (composed through ``{{> name}}`` the same as a prompt's
own, unescaped — it is prompt text) and stores the result at
``<name>.value``. No adapter is built, no tokens are counted, and there is
nothing to retry: a render failure goes through ``on_error`` exactly like
any other effect's failure, the same contract :mod:`circuitry.core.prompt`
gives a template that fails to render.
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

from ..cli.redaction import redact
from ..output import console as _console
from .prompt_compose import (
    declared_prompts,
    known_effect_names,
    render_with_composition,
)
from .store import Store


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _elapsed_str(seconds: float) -> str:
    if seconds >= 1:
        return f"{seconds:.2f}s"
    return f"{seconds * 1000:.0f}ms"


@dataclass(frozen=True)
class YieldDefinition:
    """A ``yield`` effect: render ``template`` and store the text, no model call.

    Model-only keys (``prompt_type``, ``schema``, ``model``, ``provider``,
    ``provider_fallbacks``, ``params``, ``retries``, ``timeout_ms``,
    ``deterministic``, ``assets``, ``group``, ``messages``) are rejected by
    the compiler before a ``YieldDefinition`` is ever built — see
    ``core.compiler``.
    """

    name: str
    template: str

    # Prompt-local structured values, same contract as ``PromptDefinition.inputs``.
    inputs: dict[str, Any] | None = None

    on_error: Any = "fail"  # Literal["fail", "skip", "continue"]
    description: str | None = None

    # False = skip execution and write a disabled node (see core.disabled).
    enabled: bool = True


class YieldRuntime:
    """Executes a :class:`YieldDefinition` against ``store``/``ctx``.

    Writes ``<name>.value`` (the rendered text, or ``None`` on a failure
    absorbed by ``on_error``) and ``<name>.meta{created_at, completed_at,
    error, dry_run}`` — the same minimal shape a dry-run tool/prompt leaf
    leaves behind, with nothing model-specific to report.
    """

    def __init__(
        self,
        definition: YieldDefinition,
        *,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        verbose: bool = False,
        depth: int = 0,
        cb_start: Any = None,
        cb_done: Any = None,
        cb_error: Any = None,
        display_name: str | None = None,
        ancestors: list | None = None,
    ) -> None:
        self.defn = definition
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.verbose = verbose
        self.depth = depth
        self.cb_start = cb_start
        self.cb_done = cb_done
        self.cb_error = cb_error
        self.display_name = display_name or definition.name
        self._ancestors = ancestors or []

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = node.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            node["meta"] = meta

        effective_ctx = dict(ctx)
        if self.defn.inputs:
            effective_ctx.update(self.defn.inputs)

        meta["created_at"] = _now_iso()
        meta["completed_at"] = None
        meta["error"] = None
        meta["dry_run"] = self.dry_run

        indent = "  " * self.depth
        store.fire_effect_start(self.defn.name, node)
        if self.verbose:
            if self.cb_start is not None:
                self.cb_start()
            else:
                _console.print(
                    f"{indent}[info]\u2192[/info] [magenta]\u25cb[/magenta] {self.display_name}"
                )

        t0 = time.monotonic()
        try:
            if self.dry_run:
                node["value"] = None
                meta["completed_at"] = _now_iso()
                self._announce_done(indent, t0, dry=True)
                store.fire_effect_complete(self.defn.name, node)
                return

            rendered = render_with_composition(
                self.defn.template,
                effective_ctx,
                declared=declared_prompts(self.runtime_config),
                known_effect_names=known_effect_names(self.runtime_config),
                label=f"yield '{self.defn.name}' template",
                escape=False,
            )
        except Exception as e:
            meta["error"] = redact(str(e))
            meta["completed_at"] = _now_iso()
            node["value"] = None
            self._announce_done(indent, t0, dry=False, error=True)
            store.fire_effect_complete(self.defn.name, node)
            if self.defn.on_error == "fail":
                raise
            return

        node["value"] = rendered
        meta["completed_at"] = _now_iso()
        self._announce_done(indent, t0, dry=False)
        store.fire_effect_complete(self.defn.name, node)

    def _announce_done(
        self, indent: str, t0: float, *, dry: bool, error: bool = False
    ) -> None:
        if not self.verbose:
            return
        elapsed = time.monotonic() - t0
        suffix = f" ({_elapsed_str(elapsed)}" + (", dry)" if dry else ")")
        if error:
            line = f"{indent}[err]\u2717[/err] [magenta]\u25cb[/magenta] {self.display_name} {suffix}"
            if self.cb_error is not None:
                self.cb_error(line)
            else:
                _console.print(line)
            return
        line = f"{indent}[ok]\u2713[/ok] [magenta]\u25cb[/magenta] {self.display_name} {suffix}"
        if self.cb_done is not None:
            self.cb_done(line)
        else:
            _console.print(line)
