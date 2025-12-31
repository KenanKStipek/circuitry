from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from .store import Store
from ..adapters import Adapter

if TYPE_CHECKING:
    # Type-only import to avoid circular imports at runtime
    from .dynamic import DynamicDefinition


@dataclass(frozen=True)
class ReflectorDefinition:
    name: str
    inner: "DynamicDefinition"


class ReflectorRuntime:
    def __init__(
        self,
        definition: ReflectorDefinition,
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
        # Local import to avoid circular import at module load time
        from .dynamic import DynamicRuntime

        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        node.setdefault("meta", {})

        inner_store = Store(node, on_write=store.on_write)

        DynamicRuntime(
            self.defn.inner,
            adapter=self.adapter,
            model=self.model,
            dry_run=self.dry_run,
            timeout_seconds=self.timeout_seconds,
        ).execute(store=inner_store)

        node["value"] = True
