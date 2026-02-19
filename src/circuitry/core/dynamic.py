from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any, Literal, Sequence, Union

from ..adapters import Adapter
from .prompt import PromptDefinition, PromptRuntime
from .store import Store

if TYPE_CHECKING:
    # Type-only imports to avoid circular imports at runtime
    from .conditional import ConditionalDefinition
    from .loop import LoopDefinition
    from .reflector import ReflectorDefinition


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
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds

    def execute(
        self, *, store: Store, ctx_override: dict[str, Any] | None = None
    ) -> None:
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

        ctx = ctx_override if ctx_override is not None else store.state
        child_store = Store(dyn, on_write=store.on_write)

        try:
            if self.defn.flow == "chain":
                for idx, effect in enumerate(self.defn.effects):
                    effect_path = self._effect_path(effect=effect, index=idx)
                    try:
                        self._execute_effect(effect, store=child_store, ctx=ctx)
                    except Exception as e:
                        raise RuntimeError(f"{effect_path}: {e}") from e
            else:
                # Tree semantics: each sibling effect evaluates against the same
                # deterministic snapshot from dynamic start, not sibling writes.
                tree_ctx = deepcopy(ctx)
                for idx, effect in enumerate(self.defn.effects):
                    effect_path = self._effect_path(effect=effect, index=idx)
                    try:
                        self._execute_effect(effect, store=child_store, ctx=tree_ctx)
                    except Exception as e:
                        raise RuntimeError(f"{effect_path}: {e}") from e

            dyn["value"] = True
            meta["completed_at"] = _now_iso()

        except Exception as e:
            dyn["value"] = False
            meta["error"] = str(e)
            meta["completed_at"] = _now_iso()
            raise

    def _execute_effect(
        self, effect: EffectDef, *, store: Store, ctx: dict[str, Any]
    ) -> None:
        """Execute a single effect within the dynamic."""
        if isinstance(effect, PromptDefinition):
            PromptRuntime(
                effect,
                adapter=self.adapter,
                model=self.model,
                runtime_config=self.runtime_config,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
            ).execute(store=store, ctx=ctx)

        elif isinstance(effect, DynamicDefinition):
            DynamicRuntime(
                effect,
                adapter=self.adapter,
                model=self.model,
                runtime_config=self.runtime_config,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
            ).execute(store=store, ctx_override=ctx)

        else:
            # Local imports to avoid circular imports at module load time
            from .conditional import ConditionalDefinition, ConditionalRuntime
            from .loop import LoopDefinition, LoopRuntime
            from .reflector import ReflectorDefinition, ReflectorRuntime

            if isinstance(effect, ReflectorDefinition):
                ReflectorRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=store)

            elif isinstance(effect, ConditionalDefinition):
                ConditionalRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=store, ctx=ctx)

            elif isinstance(effect, LoopDefinition):
                LoopRuntime(
                    effect,
                    adapter=self.adapter,
                    model=self.model,
                    runtime_config=self.runtime_config,
                    dry_run=self.dry_run,
                    timeout_seconds=self.timeout_seconds,
                ).execute(store=store, ctx=ctx)

            else:
                raise TypeError(f"Unsupported effect type: {type(effect)}")

    def _effect_path(self, *, effect: EffectDef, index: int) -> str:
        name = getattr(effect, "name", None)
        if isinstance(name, str) and name:
            return f"{self.defn.name}.{name}"
        return f"{self.defn.name}.{type(effect).__name__}[{index}]"
