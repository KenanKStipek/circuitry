from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Literal, Sequence, TYPE_CHECKING, Union

from .store import Store
from .prompt import PromptDefinition, PromptRuntime
from ..adapters import Adapter

if TYPE_CHECKING:
    # Type-only imports to avoid circular imports at runtime
    from .reflector import ReflectorDefinition
    from .conditional import ConditionalDefinition
    from .loop import LoopDefinition


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


EffectDef = Union[
    "DynamicDefinition",
    PromptDefinition,
    "ReflectorDefinition",
    "ConditionalDefinition",
    "LoopDefinition",
]


@dataclass(frozen=True)
class DynamicDefinition:
    name: str
    effects: Sequence[EffectDef]
    flow: Literal["chain", "tree"] = "chain"


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
                "flow": self.defn.flow,
                "dry_run": self.dry_run,
            }
        )

        if self.defn.flow not in ("chain", "tree"):
            meta["error"] = f"Unsupported flow: {self.defn.flow}"
            meta["completed_at"] = _now_iso()
            dyn["value"] = False
            raise ValueError(meta["error"])

        ctx = store.state
        child_store = Store(dyn, on_write=store.on_write)

        try:
            for effect in self.defn.effects:
                self._execute_effect(effect, store=child_store, ctx=ctx)

            dyn["value"] = True
            meta["completed_at"] = _now_iso()

        except Exception as e:
            dyn["value"] = False
            meta["error"] = str(e)
            meta["completed_at"] = _now_iso()
            raise

    def _execute_effect(self, effect: EffectDef, *, store: Store, ctx: dict) -> None:
        """Execute a single effect within the dynamic."""
        if isinstance(effect, PromptDefinition):
            PromptRuntime(
                effect,
                adapter=self.adapter,
                model=self.model,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
            ).execute(store=store, ctx=ctx)

        elif isinstance(effect, DynamicDefinition):
            DynamicRuntime(
                effect,
                adapter=self.adapter,
                model=self.model,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
            ).execute(store=store)

        else:
            # Local imports to avoid circular imports at module load time
            from .reflector import ReflectorDefinition, ReflectorRuntime
            from .conditional import ConditionalDefinition, ConditionalRuntime
            from .loop import LoopDefinition, LoopRuntime

            if isinstance(effect, ReflectorDefinition):
                ReflectorRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=store)

            elif isinstance(effect, ConditionalDefinition):
                ConditionalRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=store, ctx=ctx)

            elif isinstance(effect, LoopDefinition):
                LoopRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=store, ctx=ctx)

            else:
                raise TypeError(f"Unsupported effect type: {type(effect)}")
