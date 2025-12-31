from __future__ import annotations

from typing import Any

from .ollama import OllamaAdapter
from .base import Adapter


def build_adapter(*, adapter_name: str, runtime: dict[str, Any]) -> Adapter:
    adapter_name = (adapter_name or "").strip()

    if adapter_name == "ollama":
        adapters = (runtime or {}).get("adapters") or {}
        cfg = adapters.get("ollama") or {}
        base_url = cfg.get("base_url") or "http://localhost:11434"
        return OllamaAdapter(base_url=base_url)

    raise ValueError(f"Unknown adapter: {adapter_name!r}")
