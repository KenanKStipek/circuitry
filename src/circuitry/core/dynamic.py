from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Literal, Sequence, TYPE_CHECKING, Union

from .store import Store
from .prompt import PromptDefinition, PromptRuntime
from ..adapters import Adapter

if TYPE_CHECKING:
    # Type-only import to avoid circular imports at runtime
    from .reflector import ReflectorDefinition


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


StepDef = Union["DynamicDefinition", PromptDefinition, "ReflectorDefinition"]


@dataclass(frozen=True)
class DynamicDefinition:
    name: str
    steps: Sequence[StepDef]
    strategy: Literal["chain"] = "chain"


class DynamicRuntime:
    def __init__(
        self,
        definition: DynamicDefinition,
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

    def execute(self, *, store: Store) -> None:
        dyn = store.ensure_dict(self.defn.name)
        dyn.setdefault("value", None)
        meta = dyn.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            dyn["meta"] = meta

        meta.update(
            {
                "created_at": _now_iso(),
                "completed_at": None,
                "adapter": getattr(self.adapter, "name", "unknown"),
                "model": self.model,
                "tokens_sent": None,
                "tokens_received": None,
                "error": None,
                "strategy": self.defn.strategy,
                "dry_run": self.dry_run,
            }
        )

        if self.defn.strategy != "chain":
            meta["error"] = f"Unsupported strategy: {self.defn.strategy}"
            meta["completed_at"] = _now_iso()
            dyn["value"] = False
            raise ValueError(meta["error"])

        ctx = store.state
        child_store = Store(dyn, on_write=store.on_write)

        try:
            for step in self.defn.steps:
                if isinstance(step, PromptDefinition):
                    PromptRuntime(
                        step,
                        adapter=self.adapter,
                        model=self.model,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                    ).execute(store=child_store, ctx=ctx)

                elif isinstance(step, DynamicDefinition):
                    DynamicRuntime(
                        step,
                        adapter=self.adapter,
                        model=self.model,
                        dry_run=self.dry_run,
                        timeout_seconds=self.timeout_seconds,
                    ).execute(store=child_store)

                else:
                    # Local import to avoid circular import at module load time
                    from .reflector import ReflectorDefinition, ReflectorRuntime

                    if isinstance(step, ReflectorDefinition):
                        ReflectorRuntime(
                            step,
                            adapter=self.adapter,
                            model=self.model,
                            dry_run=self.dry_run,
                            timeout_seconds=self.timeout_seconds,
                        ).execute(store=child_store)
                    else:
                        raise TypeError(f"Unsupported step type: {type(step)}")

            dyn["value"] = True
            meta["completed_at"] = _now_iso()

        except Exception as e:
            dyn["value"] = False
            meta["error"] = str(e)
            meta["completed_at"] = _now_iso()
            raise
