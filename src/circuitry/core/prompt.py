from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Optional

from .store import Store
from ..adapters import Adapter  # uses your adapters package


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _render(template: str, ctx: dict[str, Any]) -> str:
    try:
        import chevron  # type: ignore

        return chevron.render(template, ctx)
    except Exception:
        return template


@dataclass(frozen=True)
class PromptDefinition:
    name: str
    template: str
    # optional overrides later (model, adapter, etc.)
    model: Optional[str] = None
    adapter: Optional[str] = None


class PromptRuntime:
    """
    Executes a PromptDefinition against adapter + store.
    Writes:
      <name>.value
      <name>.meta{created_at, completed_at, adapter, model, prompt_sent, tokens_sent, tokens_received, error}
    """

    def __init__(
        self,
        definition: PromptDefinition,
        *,
        adapter: Adapter,
        model: str,
        dry_run: bool = False,
        timeout_seconds: int = 120,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = node.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            node["meta"] = meta

        prompt_sent = _render(self.defn.template, ctx)

        # contract
        meta["created_at"] = _now_iso()
        meta["completed_at"] = None
        meta["adapter"] = getattr(self.adapter, "name", "unknown")
        meta["model"] = self.model
        meta["prompt_sent"] = prompt_sent
        meta["tokens_sent"] = None
        meta["tokens_received"] = None
        meta["error"] = None
        meta["dry_run"] = self.dry_run

        if self.dry_run:
            node["value"] = None
            meta["completed_at"] = _now_iso()
            return

        try:
            res = self.adapter.generate(
                model=self.model,
                prompt=prompt_sent,
                timeout_seconds=self.timeout_seconds,
            )
            node["value"] = res.text
            meta["tokens_sent"] = res.tokens_sent
            meta["tokens_received"] = res.tokens_received
            meta["completed_at"] = _now_iso()
        except Exception as e:
            meta["error"] = str(e)
            meta["completed_at"] = _now_iso()
            raise
